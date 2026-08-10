//! The port's C ABI, exposed to the harness as safe Rust.
//!
//! # Why this module exists
//!
//! `crates/libz-rs-sys` exports the zlib C ABI, so every pointer-taking entry point is
//! `pub unsafe extern "C" fn` and calling one is an `unsafe` operation. The harness -- the
//! differential suites, the three benches and the five fuzz targets -- has to call all of them,
//! and until this module existed each of those files opened its own `unsafe` block to do it:
//! measured at the time, 380 blocks spread across ten files, with the same gate written three and
//! four times over.
//!
//! Unsafe containment is a stated constraint of this project, not a preference: `unsafe` belongs
//! in a narrowly scoped, documented C-ABI boundary and nowhere else. This module is the port half
//! of that boundary for the dev-only harness and [`crate::oracle`] is the reference half. Every
//! other file that could hold `unsafe` is held to not holding it by the compiler: `build.rs`,
//! every suite under `tests/`, all three attached benches and all five `fuzz_targets/` carry
//! `#![forbid(unsafe_code)]`. The crate's own `src/lib.rs` is the single exception and cannot be
//! otherwise -- a crate-root `forbid` would cover this module too -- which is why it holds
//! documentation and two `pub mod` declarations and no code at all.
//!
//! Nothing here decides anything. Each function installs no policy, asserts nothing and adds no
//! behaviour: it converts Rust's safe shapes into the C shapes the ABI takes, makes exactly one
//! call, and converts the answer back. The chunk boundaries, flush values, corpora and
//! comparisons all stay in the harness, which is what keeps a difference in behaviour
//! attributable to the implementations rather than to this file.
//!
//! # The obligations every gate here discharges
//!
//! Each `// SAFETY:` comment below names the subset that applies to it by letter.
//!
//! * **(a) The stream.** A `&mut z_stream` is by construction non-null, aligned and unique, and
//!   points at a `z_stream` the harness owns for longer than the call. Where a null
//!   `z_streamp` is a *documented* argument -- [`crate::port::deflate_bound`] and
//!   [`crate::port::deflate_bound_z`] funnel
//!   through `deflateStateCheck`, which is a null test before it is anything else -- the
//!   parameter is `Option<&mut z_stream>` and `None` is what produces the null.
//! * **(b) The state.** Every stream handed to these functions was initialised by this side's own
//!   `deflateInit2_`/`inflateInit2_`/`inflateBackInit_`, or not at all; no stream ever crosses to
//!   the other implementation's functions. The entry point's own state check accepts or rejects
//!   it exactly as it would for a C caller.
//! * **(c) Readable input.** Where a `(pointer, length)` pair is formed from a `&[u8]`, the
//!   length is that slice's own and the slice outlives the call. A zero-length window is passed
//!   as a null pointer with length zero, the pairing `zlib.h` L91 permits explicitly.
//! * **(d) Writable output.** Where a pair is formed from a `&mut [u8]`, the length is that
//!   slice's own, the borrow is exclusive, and the slice outlives the call.
//! * **(e) Version and size.** Initialisation always passes this side's own `ZLIB_VERSION` and
//!   `size_of::<z_stream>()`; crossing them with the other side's would answer `Z_VERSION_ERROR`
//!   (`zlib.h` L248) rather than produce a working stream.
//! * **(f) Out-parameters.** Formed with `addr_of_mut!` over live locals of exactly the declared
//!   type, initialised before the call, outliving it, and never retained by the library.
//! * **(g) C strings and handles.** Every `&CStr` is NUL-terminated and read-only for the
//!   library. A [`crate::port::GzFile`] can only have come from this module's own
//!   `gzopen`/`gzopen64`/`gzdopen` gates, and the harness closes it exactly once -- or it is the
//!   `Z_NULL` of [`crate::port::GzFile::null`], which every `gz*` entry point answers from a
//!   guard at its head without dereferencing anything and which therefore owes no close. Where a
//!   null *string* is the documented argument, the null is formed inside a gate of its own
//!   ([`crate::port::GzFile::open_null_path`] and its two siblings) because `&CStr` cannot
//!   express one.
//!
//! # What this module must never do
//!
//! * **Never `#[no_mangle]` anything.** This crate exports no C symbol; a second definition of
//!   any of these names would break the link it exists to perform.
//! * **Never `extern "C-unwind"`.** Only the non-unwinding convention is used here and in
//!   [`crate::oracle`], so a panic reaching the boundary aborts instead of becoming undefined
//!   behaviour in a frame compiled without unwind tables. The callback bodies in
//!   [`crate::port::TrackingAllocator`] and the [`crate::port::inflate_back`] bridge are the ones
//!   the library actually
//!   calls, and neither contains a reachable panic.
//! * **Never assert that the library answered correctly.** That judgement, and the message that
//!   goes with it, belong to the harness: this is library code in a crate whose workspace lints
//!   deny `unwrap_used`, `expect_used`, `panic` and `indexing_slicing` outside `#[cfg(test)]`.
//!   The line is what the caller can see for itself. A gate may assert about something only
//!   observable *inside* it -- the pointer `gzgets` returned, which is out of scope by the time
//!   the caller has the answer ([`crate::port::GzFile::gets`]), and the guard bytes around a
//!   [`crate::port::GuardedBuf`], which are reachable only through that buffer's own pointer --
//!   and it must refuse an argument it cannot forward soundly, because a safe function has to be
//!   sound for every argument it accepts ([`crate::port::GuardedBuf::new`] on a capacity that
//!   cannot carry its guards, and [`crate::oracle::adler32_null`] on the one length at which the
//!   reference dereferences a null pointer). Each of those documents itself under `# Panics`.

use core::ffi::{c_char, c_int, c_long, c_uint, c_void};
use std::alloc::{alloc, dealloc, Layout};
use std::ffi::CStr;
use std::ptr;

use libz_rs_sys::{
    gzFile, gz_header, in_func, out_func, uInt, uLong, voidpf, z_crc_t, z_off64_t, z_off_t,
    z_size_t, z_stream, Bytef, ZLIB_VERSION,
};

/// This side's `stream_size` argument: `sizeof(z_stream)` as the initialisers take it.
///
/// Obligation (e). Saturating rather than `as`, so a hypothetical target where the size did not
/// fit in a `c_int` would report the largest representable value instead of a truncated one.
fn stream_size() -> c_int {
    c_int::try_from(size_of::<z_stream>()).unwrap_or(c_int::MAX)
}

/// A `z_stream` with every field zeroed and both allocator hooks left null.
///
/// The `memset`-then-`deflateInit` sequence `test/example.c` performs, which selects the
/// library's own internal default allocator. Built field by field rather than through
/// `MaybeUninit::zeroed`, so it needs no `unsafe` at all: `z_stream` deliberately implements
/// neither `Default` nor `Zeroable`, because a zeroed stream is not yet a valid one.
#[must_use]
pub fn zeroed_stream() -> z_stream {
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

/// `len` as a `uInt`, saturating rather than truncating.
fn narrow_uint(len: usize) -> uInt {
    uInt::try_from(len).unwrap_or(uInt::MAX)
}

/// `len` as a `uLong`, saturating rather than truncating.
fn narrow_ulong(len: usize) -> uLong {
    uLong::try_from(len).unwrap_or(uLong::MAX)
}

/// `len` as a `c_int`, saturating rather than truncating.
fn narrow_int(len: usize) -> c_int {
    c_int::try_from(len).unwrap_or(c_int::MAX)
}

// =================================================================================================
//  deflate
// =================================================================================================

/// `deflateInit2_(strm, level, method, windowBits, memLevel, strategy, ZLIB_VERSION, size)`.
#[must_use]
pub fn deflate_init2(
    strm: &mut z_stream,
    level: c_int,
    method: c_int,
    window_bits: c_int,
    mem_level: c_int,
    strategy: c_int,
) -> c_int {
    // SAFETY: obligations (a), (b) and (e). The entry point reads at most one byte of `version`
    // after testing it for null, and `ZLIB_VERSION` is a `&CStr` constant with static storage
    // duration; it writes `state` and touches neither window, which is why a C caller may leave
    // `next_in`/`next_out` unset until after the call.
    unsafe {
        libz_rs_sys::deflateInit2_(
            strm,
            level,
            method,
            window_bits,
            mem_level,
            strategy,
            ZLIB_VERSION.as_ptr(),
            stream_size(),
        )
    }
}

/// `deflateInit_(strm, level, ZLIB_VERSION, size)`.
#[must_use]
pub fn deflate_init(strm: &mut z_stream, level: c_int) -> c_int {
    // SAFETY: as [`deflate_init2`] -- obligations (a), (b) and (e).
    unsafe { libz_rs_sys::deflateInit_(strm, level, ZLIB_VERSION.as_ptr(), stream_size()) }
}

/// `deflateInit2_` with a deliberately wrong version string, which must answer
/// `Z_VERSION_ERROR`.
///
/// Obligation (e) inverted: the major-version comparison at `zlib.h` L248 is a documented part
/// of the contract, and the only way to reach it is to pass a version whose first byte differs.
#[must_use]
pub fn deflate_init2_with_version(
    strm: &mut z_stream,
    level: c_int,
    method: c_int,
    window_bits: c_int,
    mem_level: c_int,
    strategy: c_int,
    version: &CStr,
) -> c_int {
    // SAFETY: obligations (a), (b) and (g). `version` is NUL-terminated and outlives the call;
    // the library reads its first byte and nothing else.
    unsafe {
        libz_rs_sys::deflateInit2_(
            strm,
            level,
            method,
            window_bits,
            mem_level,
            strategy,
            version.as_ptr(),
            stream_size(),
        )
    }
}

/// `deflate(strm, flush)`, with the two windows left exactly as the caller installed them.
#[must_use]
pub fn deflate(strm: &mut z_stream, flush: c_int) -> c_int {
    // SAFETY: obligations (a) through (d). This is the one gate that relies on all four,
    // because it is the one that reads through `next_in` and writes through `next_out`; the
    // caller installs both from slices that outlive the call.
    unsafe { libz_rs_sys::deflate(strm, flush) }
}

/// `deflateEnd(strm)`.
#[must_use]
pub fn deflate_end(strm: &mut z_stream) -> c_int {
    // SAFETY: obligations (a) and (b). The state block is released through the same allocator it
    // was taken from and `state` is left null, so a second call is rejected rather than freeing
    // twice.
    unsafe { libz_rs_sys::deflateEnd(strm) }
}

/// `deflateReset(strm)`.
#[must_use]
pub fn deflate_reset(strm: &mut z_stream) -> c_int {
    // SAFETY: obligations (a) and (b). The state block is reinitialised in place and the
    // allocations it holds are kept.
    unsafe { libz_rs_sys::deflateReset(strm) }
}

/// `deflateResetKeep(strm)`.
#[must_use]
pub fn deflate_reset_keep(strm: &mut z_stream) -> c_int {
    // SAFETY: obligations (a) and (b), as [`deflate_reset`].
    unsafe { libz_rs_sys::deflateResetKeep(strm) }
}

/// `deflateInit2_(Z_NULL, ..., version, stream_size)` -- the version-gate-ordering probe.
///
/// ★ The ordering is the assertion. `deflate.c` L394-L397 answers `Z_VERSION_ERROR` *before* it
/// examines `strm == Z_NULL` at L398, so a null stream paired with an unacceptable handshake must
/// answer `Z_VERSION_ERROR` and a null stream with an acceptable one must answer
/// `Z_STREAM_ERROR`. Neither is expressible through [`deflate_init2`]: obligation (a) makes a
/// `&mut z_stream` non-null, obligation (e) fixes the version and the size, and `&CStr` cannot
/// spell a null version. `None` for `version` is that null; `stream_size` is passed through
/// unchanged so a deliberately wrong size can be probed.
#[must_use]
pub fn deflate_init2_null_stream(
    level: c_int,
    method: c_int,
    window_bits: c_int,
    mem_level: c_int,
    strategy: c_int,
    version: Option<&CStr>,
    stream_size: c_int,
) -> c_int {
    let version = match version {
        Some(version) => version.as_ptr(),
        None => ptr::null(),
    };
    // SAFETY: the stream pointer is deliberately null, which is a value the contract defines
    // rather than a violation of it -- the entry point tests it and never dereferences it. The
    // version pointer is either null or a `&CStr`'s, so it addresses a readable NUL-terminated
    // string that outlives the call.
    unsafe {
        libz_rs_sys::deflateInit2_(
            ptr::null_mut(),
            level,
            method,
            window_bits,
            mem_level,
            strategy,
            version,
            stream_size,
        )
    }
}

/// `deflateSetDictionary(strm, dictionary, dictLength)`.
///
/// The slice's own pointer is passed verbatim, empty slice included, and that is not the same
/// call as one with `Z_NULL` in that position: `deflate.c` L586 refuses a null dictionary with
/// `Z_STREAM_ERROR` outright, whereas a non-null pointer with a zero length is an ordinary
/// zero-byte dictionary and succeeds. An empty slice yields a dangling-but-aligned non-null
/// pointer, which is the second of those, and it is what a C caller with an empty buffer passes.
#[must_use]
pub fn deflate_set_dictionary(strm: &mut z_stream, dictionary: &[u8]) -> c_int {
    // SAFETY: obligations (a), (b) and (c) -- `dictionary.len()` bytes are readable at
    // `dictionary.as_ptr()` for the whole call. The library copies what it keeps into its own
    // window and retains no pointer into `dictionary`.
    unsafe {
        libz_rs_sys::deflateSetDictionary(strm, dictionary.as_ptr(), narrow_uint(dictionary.len()))
    }
}

/// `deflateSetDictionary(strm, dictionary, dictLength)`, where `None` passes the null `z_streamp`.
///
/// The null stream is a documented argument here rather than a violation: `deflateStateCheck`
/// (`deflate.c` L876) is a null test before it is anything else, so the entry point answers
/// `Z_STREAM_ERROR` without a dereference. Passing null is the only way to reach that branch,
/// which is why there is a gate for it instead of a reason to avoid it.
#[must_use]
pub fn deflate_set_dictionary_of(strm: Option<&mut z_stream>, dictionary: &[u8]) -> c_int {
    let strm = strm.map_or(ptr::null_mut(), ptr::from_mut);
    // SAFETY: obligations (a) -- including its null case -- (b) and (c). `dictionary.len()` bytes
    // are readable at `dictionary.as_ptr()` for the whole call and the library retains no pointer
    // into them; with a null stream nothing is dereferenced at all.
    unsafe {
        libz_rs_sys::deflateSetDictionary(strm, dictionary.as_ptr(), narrow_uint(dictionary.len()))
    }
}

/// `deflateGetDictionary(strm, dictionary, &dictLength)`, as the status and the length written.
///
/// A `None` destination is C's `dictionary == Z_NULL`, which asks for the length alone.
#[must_use]
pub fn deflate_get_dictionary(strm: &mut z_stream, dictionary: Option<&mut [u8]>) -> (c_int, uInt) {
    let mut length: uInt = 0;
    let (ptr, _) = match dictionary {
        Some(buf) => (buf.as_mut_ptr(), buf.len()),
        None => (ptr::null_mut(), 0),
    };
    // SAFETY: obligations (a), (b), (d) and (f). The library writes at most `dictLength` bytes,
    // and it learns that cap from the value it writes back rather than from an input -- so the
    // destination must be at least `MAX_DIST`-sized, which every caller here satisfies by
    // passing a window-sized buffer. `length` is a live local the library either writes or
    // leaves alone.
    let status =
        unsafe { libz_rs_sys::deflateGetDictionary(strm, ptr, core::ptr::addr_of_mut!(length)) };
    (status, length)
}

/// `deflateSetHeader(strm, head)`.
///
/// The header stays borrowed for as long as the library holds it -- `deflate.c` keeps the
/// pointer until the header has been emitted -- which the exclusive borrow expresses.
#[must_use]
pub fn deflate_set_header(strm: &mut z_stream, head: &mut gz_header) -> c_int {
    // SAFETY: obligations (a), (b) and (d). `head` is a live, aligned `gz_header` the caller
    // owns; the library reads its members and writes `done`, and the borrow outlives the call.
    unsafe { libz_rs_sys::deflateSetHeader(strm, head) }
}

/// `deflateParams(strm, level, strategy)`.
#[must_use]
pub fn deflate_params(strm: &mut z_stream, level: c_int, strategy: c_int) -> c_int {
    // SAFETY: obligations (a) through (d): a level change can flush pending output, so the
    // windows are read and written exactly as `deflate` does.
    unsafe { libz_rs_sys::deflateParams(strm, level, strategy) }
}

/// `deflatePrime(strm, bits, value)`.
#[must_use]
pub fn deflate_prime(strm: &mut z_stream, bits: c_int, value: c_int) -> c_int {
    // SAFETY: obligations (a) and (b). Only the pending-output buffer is touched.
    unsafe { libz_rs_sys::deflatePrime(strm, bits, value) }
}

/// `deflateTune(strm, good_length, max_lazy, nice_length, max_chain)`.
#[must_use]
pub fn deflate_tune(
    strm: &mut z_stream,
    good_length: c_int,
    max_lazy: c_int,
    nice_length: c_int,
    max_chain: c_int,
) -> c_int {
    // SAFETY: obligations (a) and (b). Four integers are written into the state block.
    unsafe { libz_rs_sys::deflateTune(strm, good_length, max_lazy, nice_length, max_chain) }
}

/// `deflateCopy(dest, source)`.
#[must_use]
pub fn deflate_copy(dest: &mut z_stream, source: &mut z_stream) -> c_int {
    // SAFETY: obligations (a) and (b) for both streams, which are distinct exclusive borrows and
    // therefore cannot alias -- the one thing `deflate.c`'s copy cannot tolerate.
    unsafe { libz_rs_sys::deflateCopy(dest, source) }
}

/// `deflatePending(strm, &pending, &bits)`, as the status and the two out-parameters.
#[must_use]
pub fn deflate_pending(strm: &mut z_stream, bytes: &mut c_uint, bits: &mut c_int) -> c_int {
    // SAFETY: obligations (a), (b) and (f). `addr_of_mut!` rather than passing the references
    // through, because these are out-parameters C writes through and the raw pointer is taken
    // straight from the place instead of materialising a Rust reference whose aliasing rules C
    // is under no obligation to respect. Neither pointer is retained past the call.
    unsafe {
        libz_rs_sys::deflatePending(
            strm,
            core::ptr::addr_of_mut!(*bytes),
            core::ptr::addr_of_mut!(*bits),
        )
    }
}

/// `deflatePending(strm, Z_NULL, Z_NULL)` -- the both-out-parameters-null face.
///
/// `deflate.c` L679-L684 makes each out-parameter optional, so a caller that wants neither is
/// asking a legitimate question -- whether the stream is in a state where pending output can be
/// reported at all -- and nothing but a deliberate null pair exercises it.
#[must_use]
pub fn deflate_pending_null(strm: &mut z_stream) -> c_int {
    // SAFETY: obligations (a) and (b). Both out-parameters are null, which the entry point tests
    // before writing through either.
    unsafe { libz_rs_sys::deflatePending(strm, ptr::null_mut(), ptr::null_mut()) }
}

/// `deflateUsed(strm, &bits)`, as the status and the out-parameter.
#[must_use]
pub fn deflate_used(strm: &mut z_stream, bits: &mut c_int) -> c_int {
    // SAFETY: obligations (a), (b) and (f), as [`deflate_pending`].
    unsafe { libz_rs_sys::deflateUsed(strm, core::ptr::addr_of_mut!(*bits)) }
}

/// `deflateUsed(strm, Z_NULL)` -- the null-out-parameter face, as [`deflate_pending_null`].
#[must_use]
pub fn deflate_used_null(strm: &mut z_stream) -> c_int {
    // SAFETY: obligations (a) and (b). The out-parameter is null, which the entry point tests
    // before writing through it.
    unsafe { libz_rs_sys::deflateUsed(strm, ptr::null_mut()) }
}

/// `deflateBound(strm, sourceLen)`, where `None` passes the null `z_streamp`.
#[must_use]
pub fn deflate_bound(strm: Option<&mut z_stream>, source_len: uLong) -> uLong {
    let strm = strm.map_or(ptr::null_mut(), ptr::from_mut);
    // SAFETY: obligation (a), including its null case. The entry point reads `wrap`,
    // `strstart`, `w_bits` and `hash_bits` out of the state block, writes nothing, and touches
    // no window; with a null stream it funnels through `deflateStateCheck`'s null test and
    // answers the conservative bound without a dereference.
    unsafe { libz_rs_sys::deflateBound(strm, source_len) }
}

/// `deflateBound_z(strm, sourceLen)`, where `None` passes the null `z_streamp`.
#[must_use]
pub fn deflate_bound_z(strm: Option<&mut z_stream>, source_len: z_size_t) -> z_size_t {
    let strm = strm.map_or(ptr::null_mut(), ptr::from_mut);
    // SAFETY: obligation (a), including its null case, as [`deflate_bound`].
    unsafe { libz_rs_sys::deflateBound_z(strm, source_len) }
}

// =================================================================================================
//  The one-shot wrappers
// =================================================================================================

/// `compress(dest, &destLen, source, sourceLen)`, as the status and `*destLen`.
#[must_use]
pub fn compress(dest: &mut [u8], source: &[u8]) -> (c_int, uLong) {
    let mut dest_len = narrow_ulong(dest.len());
    // SAFETY: obligations (c), (d) and (f). `destLen` is an in/out parameter (`compress.c`
    // L14-L16), so the pointer is taken straight from the place; the two slices are distinct
    // borrows and therefore cannot overlap, and neither pointer is retained.
    let status = unsafe {
        libz_rs_sys::compress(
            dest.as_mut_ptr(),
            core::ptr::addr_of_mut!(dest_len),
            source.as_ptr(),
            narrow_ulong(source.len()),
        )
    };
    (status, dest_len)
}

/// `compress2(dest, &destLen, source, sourceLen, level)`, as the status and `*destLen`.
#[must_use]
pub fn compress2(dest: &mut [u8], source: &[u8], level: c_int) -> (c_int, uLong) {
    let mut dest_len = narrow_ulong(dest.len());
    // SAFETY: obligations (c), (d) and (f), as [`compress`].
    let status = unsafe {
        libz_rs_sys::compress2(
            dest.as_mut_ptr(),
            core::ptr::addr_of_mut!(dest_len),
            source.as_ptr(),
            narrow_ulong(source.len()),
            level,
        )
    };
    (status, dest_len)
}

/// `compress2_z(dest, &destLen, source, sourceLen, level)`, the `size_t` form.
#[must_use]
pub fn compress2_z(dest: &mut [u8], source: &[u8], level: c_int) -> (c_int, z_size_t) {
    let mut dest_len: z_size_t = dest.len();
    // SAFETY: obligations (c), (d) and (f), as [`compress`].
    let status = unsafe {
        libz_rs_sys::compress2_z(
            dest.as_mut_ptr(),
            core::ptr::addr_of_mut!(dest_len),
            source.as_ptr(),
            source.len(),
            level,
        )
    };
    (status, dest_len)
}

/// `uncompress(dest, &destLen, source, sourceLen)`, as the status and `*destLen`.
#[must_use]
pub fn uncompress(dest: &mut [u8], source: &[u8]) -> (c_int, uLong) {
    let mut dest_len = narrow_ulong(dest.len());
    // SAFETY: obligations (c), (d) and (f), as [`compress`].
    let status = unsafe {
        libz_rs_sys::uncompress(
            dest.as_mut_ptr(),
            core::ptr::addr_of_mut!(dest_len),
            source.as_ptr(),
            narrow_ulong(source.len()),
        )
    };
    (status, dest_len)
}

/// `uncompress2(dest, &destLen, source, &sourceLen)`, as the status, `*destLen` and `*sourceLen`.
///
/// `sourceLen` is an in/out parameter here and a by-value one in [`uncompress`]: the two-argument
/// form reports how much of the source it consumed, which is what makes it usable on a
/// concatenated stream.
#[must_use]
pub fn uncompress2(dest: &mut [u8], source: &[u8]) -> (c_int, uLong, uLong) {
    let mut dest_len = narrow_ulong(dest.len());
    let mut source_len = narrow_ulong(source.len());
    // SAFETY: obligations (c), (d) and (f). Both lengths are live locals initialised to the
    // slices' true extents, which is the in/out contract `zlib.h` L1330 documents.
    let status = unsafe {
        libz_rs_sys::uncompress2(
            dest.as_mut_ptr(),
            core::ptr::addr_of_mut!(dest_len),
            source.as_ptr(),
            core::ptr::addr_of_mut!(source_len),
        )
    };
    (status, dest_len, source_len)
}

// =================================================================================================
//  inflate
// =================================================================================================

/// `inflateInit2_(strm, windowBits, ZLIB_VERSION, size)`.
#[must_use]
pub fn inflate_init2(strm: &mut z_stream, window_bits: c_int) -> c_int {
    // SAFETY: obligations (a), (b) and (e), as [`deflate_init2`].
    unsafe { libz_rs_sys::inflateInit2_(strm, window_bits, ZLIB_VERSION.as_ptr(), stream_size()) }
}

/// `inflateInit_(strm, ZLIB_VERSION, size)`.
#[must_use]
pub fn inflate_init(strm: &mut z_stream) -> c_int {
    // SAFETY: obligations (a), (b) and (e), as [`deflate_init2`].
    unsafe { libz_rs_sys::inflateInit_(strm, ZLIB_VERSION.as_ptr(), stream_size()) }
}

/// `inflateInit2_` with a deliberately wrong version string, which must answer
/// `Z_VERSION_ERROR`.
#[must_use]
pub fn inflate_init2_with_version(
    strm: &mut z_stream,
    window_bits: c_int,
    version: &CStr,
) -> c_int {
    // SAFETY: obligations (a), (b) and (g), as [`deflate_init2_with_version`].
    unsafe { libz_rs_sys::inflateInit2_(strm, window_bits, version.as_ptr(), stream_size()) }
}

/// `inflate(strm, flush)`, with the two windows left exactly as the caller installed them.
#[must_use]
pub fn inflate(strm: &mut z_stream, flush: c_int) -> c_int {
    // SAFETY: obligations (a) through (d), as [`deflate`].
    unsafe { libz_rs_sys::inflate(strm, flush) }
}

/// `inflateEnd(strm)`.
#[must_use]
pub fn inflate_end(strm: &mut z_stream) -> c_int {
    // SAFETY: obligations (a) and (b), as [`deflate_end`].
    unsafe { libz_rs_sys::inflateEnd(strm) }
}

/// `inflateReset(strm)`.
#[must_use]
pub fn inflate_reset(strm: &mut z_stream) -> c_int {
    // SAFETY: obligations (a) and (b), as [`deflate_reset`].
    unsafe { libz_rs_sys::inflateReset(strm) }
}

/// `inflateReset2(strm, windowBits)`.
#[must_use]
pub fn inflate_reset2(strm: &mut z_stream, window_bits: c_int) -> c_int {
    // SAFETY: obligations (a) and (b). A window-size change can reallocate the window through
    // the same allocator the stream was initialised with.
    unsafe { libz_rs_sys::inflateReset2(strm, window_bits) }
}

/// `inflateResetKeep(strm)`.
#[must_use]
pub fn inflate_reset_keep(strm: &mut z_stream) -> c_int {
    // SAFETY: obligations (a) and (b), as [`inflate_reset`].
    unsafe { libz_rs_sys::inflateResetKeep(strm) }
}

/// `inflateSetDictionary(strm, dictionary, dictLength)`.
#[must_use]
pub fn inflate_set_dictionary(strm: &mut z_stream, dictionary: &[u8]) -> c_int {
    // SAFETY: obligations (a), (b) and (c). The pointer is the slice's own, empty slice
    // included, for the reason [`deflate_set_dictionary`] records: a zero-length dictionary
    // through a live pointer is a different call from one through `Z_NULL`, and it is the one a
    // C caller with an empty buffer makes. The library folds the bytes into its window and
    // retains no pointer into them.
    unsafe {
        libz_rs_sys::inflateSetDictionary(strm, dictionary.as_ptr(), narrow_uint(dictionary.len()))
    }
}

/// `inflateGetDictionary(strm, dictionary, &dictLength)`, as the status and the length written.
#[must_use]
pub fn inflate_get_dictionary(strm: &mut z_stream, dictionary: Option<&mut [u8]>) -> (c_int, uInt) {
    let mut length: uInt = 0;
    let ptr = match dictionary {
        Some(buf) => buf.as_mut_ptr(),
        None => ptr::null_mut(),
    };
    // SAFETY: obligations (a), (b), (d) and (f), as [`deflate_get_dictionary`].
    let status =
        unsafe { libz_rs_sys::inflateGetDictionary(strm, ptr, core::ptr::addr_of_mut!(length)) };
    (status, length)
}

/// `inflateGetDictionary(strm, dictionary, dictLength)` into a guard-fenced destination.
///
/// The same call as [`inflate_get_dictionary`], with the destination named as a
/// [`GuardedBuf`] rather than as a slice. That is not a convenience: `zlib.h` L936-L942
/// obliges the caller to provide a whole window's worth and gives the function no way to
/// learn the buffer's real size, so this is a place where an over-write would be a caller
/// buffer overflow, and the guarded form is what detects one. Forming a slice over the
/// allocation would defeat it -- the guard bytes must stay reachable only through the
/// buffer's own pointer.
///
/// `dictionary.capacity()` is what the library is told about, so the guards lie outside
/// everything it may legitimately touch.
#[must_use]
pub fn inflate_get_dictionary_guarded(
    strm: &mut z_stream,
    dictionary: &GuardedBuf,
) -> (c_int, uInt) {
    let mut length: uInt = 0;
    // SAFETY: obligations (a), (b) and (f). `dictionary.data()` is writable for
    // `dictionary.capacity()` bytes -- the guarded region's own allocation, which outlives this
    // call -- and the library writes at most a window's worth into it; `length` is a live,
    // aligned, writable `uInt` this frame owns and which nothing else references.
    let status = unsafe {
        libz_rs_sys::inflateGetDictionary(strm, dictionary.data(), core::ptr::addr_of_mut!(length))
    };
    (status, length)
}

/// `inflateGetHeader(strm, head)`.
///
/// The library retains the pointer until the header has been parsed and writes through the
/// `extra`, `name` and `comment` buffers it names, clamped to `extra_max`, `name_max` and
/// `comm_max` -- which is why the borrow is exclusive and why the caller keeps those buffers
/// alive for the whole decode.
#[must_use]
pub fn inflate_get_header(strm: &mut z_stream, head: &mut gz_header) -> c_int {
    // SAFETY: obligations (a), (b) and (d). `head` is live and aligned for the duration of the
    // borrow, and the buffers it points at are the caller's, sized by the three `*_max` members
    // the library clamps its writes to.
    unsafe { libz_rs_sys::inflateGetHeader(strm, head) }
}

/// `inflateSync(strm)`.
#[must_use]
pub fn inflate_sync(strm: &mut z_stream) -> c_int {
    // SAFETY: obligations (a) through (c): the search consumes input through `next_in`.
    unsafe { libz_rs_sys::inflateSync(strm) }
}

/// `inflateSyncPoint(strm)`.
#[must_use]
pub fn inflate_sync_point(strm: &mut z_stream) -> c_int {
    // SAFETY: obligations (a) and (b). Two state members are read and nothing is written.
    unsafe { libz_rs_sys::inflateSyncPoint(strm) }
}

/// `inflateCopy(dest, source)`.
#[must_use]
pub fn inflate_copy(dest: &mut z_stream, source: &mut z_stream) -> c_int {
    // SAFETY: obligations (a) and (b) for both streams, which cannot alias, as
    // [`deflate_copy`].
    unsafe { libz_rs_sys::inflateCopy(dest, source) }
}

/// `inflatePrime(strm, bits, value)`.
#[must_use]
pub fn inflate_prime(strm: &mut z_stream, bits: c_int, value: c_int) -> c_int {
    // SAFETY: obligations (a) and (b). The bit accumulator is written and no buffer is touched.
    unsafe { libz_rs_sys::inflatePrime(strm, bits, value) }
}

/// `inflateMark(strm)`.
#[must_use]
pub fn inflate_mark(strm: &mut z_stream) -> c_long {
    // SAFETY: obligations (a) and (b). Read-only.
    unsafe { libz_rs_sys::inflateMark(strm) }
}

/// `inflateCodesUsed(strm)`.
#[must_use]
pub fn inflate_codes_used(strm: &mut z_stream) -> uLong {
    // SAFETY: obligations (a) and (b). Read-only.
    unsafe { libz_rs_sys::inflateCodesUsed(strm) }
}

/// `inflateUndermine(strm, subvert)`.
#[must_use]
pub fn inflate_undermine(strm: &mut z_stream, subvert: c_int) -> c_int {
    // SAFETY: obligations (a) and (b). One state member is written.
    unsafe { libz_rs_sys::inflateUndermine(strm, subvert) }
}

/// `inflateValidate(strm, check)`.
#[must_use]
pub fn inflate_validate(strm: &mut z_stream, check: c_int) -> c_int {
    // SAFETY: obligations (a) and (b). One state member is written.
    unsafe { libz_rs_sys::inflateValidate(strm, check) }
}

// =================================================================================================
//  inflateBack -- the callback-driven decompressor
// =================================================================================================

/// The input half of the [`inflate_back`] contract: the safe counterpart of C's `in_func`.
///
/// ★ **Polarity.** C's `in()` returns a *count*, so zero means "no bytes, and there will be no
/// more" -- failure. Here that is [`None`] or an empty slice, and `inflateBack` turns it into
/// `Z_BUF_ERROR` with `next_in` left null (`infback.c` L101-L108).
///
/// The returned slice must stay valid until the next call, which is what `'i` expresses:
/// `infback.c` L172-L175 says the application must not change the provided input until `in()` is
/// called again or `inflateBack` returns.
pub trait BackSource<'i> {
    /// The next run of input, or [`None`] for "no more input".
    fn next_chunk(&mut self) -> Option<&'i [u8]>;

    /// Called *instead of* [`BackSource::next_chunk`] when the library handed `in()` an
    /// unusable out-parameter -- a null or misaligned `unsigned char **`.
    ///
    /// `zlib.h` L1134-L1135 does not permit that, so reaching this is a library defect. The
    /// default does nothing and the gate then answers `in()` with zero, which `infback.c`
    /// L101-L108 turns into `Z_BUF_ERROR` -- so the defect degrades to a refusal rather than a
    /// write through a null pointer. A harness that wants to assert on it overrides this and
    /// records the fact; recording rather than asserting is what the callback edge requires,
    /// because a panic here would have to unwind through C.
    fn note_unusable_out_parameter(&mut self) {}
}

/// The output half of the [`inflate_back`] contract: the safe counterpart of C's `out_func`.
///
/// ★ **Polarity, and it is inverted.** C's `out()` returns a *status*, so zero means accepted and
/// non-zero means failure. Here `true` accepts and `false` refuses.
pub trait BackSink {
    /// Accepts `data`, which the decoder has just written at `offset` bytes into the window it
    /// was given. Returns `false` to refuse, which is C's non-zero return.
    ///
    /// `offset` is [`None`] when the reported region does not lie inside that window, which is a
    /// defect in the library rather than in the sink -- reported rather than asserted, because
    /// this runs inside a callback C would have to unwind through.
    fn write_out(&mut self, data: &[u8], offset: Option<usize>) -> bool;
}

/// What the two callbacks are handed through `in_desc`/`out_desc`.
///
/// One allocation, holding the two trait objects and the window extent the offsets are computed
/// against. It never moves while `inflateBack` is running, because the call borrows it.
struct BackBridge<'a, 'i> {
    /// The source `pull` reads from.
    source: &'a mut dyn BackSource<'i>,

    /// The sink `push` writes to.
    sink: &'a mut dyn BackSink,

    /// Base of the window handed to `inflateBackInit_`, for the offset computation.
    window: *const u8,

    /// Length of that window: exactly `1 << windowBits`.
    window_len: usize,
}

/// C's `in()`, bridged to [`BackSource`].
///
/// # Safety
///
/// `desc` must be null or a pointer to a live [`BackBridge`] that nothing else is borrowing for
/// the duration of the call, and `buf` must be a writable `*const u8` out-parameter. Installing
/// the bridge through [`inflate_back`] and letting only the library call this satisfies both.
unsafe extern "C" fn pull(desc: *mut c_void, buf: *mut *const u8) -> c_uint {
    let handle = desc.cast::<BackBridge<'_, '_>>();
    if handle.is_null() || !handle.is_aligned() {
        // `test/infcover.c` L453-L456: a null descriptor is not an error, it is the caller
        // saying it has already staged everything in `next_in`. The alignment test is applied
        // after narrowing, because `c_void`'s alignment is one and testing it there would
        // always succeed.
        return 0;
    }

    // SAFETY: the caller guarantees `desc` addresses a live, aligned bridge that nothing else
    // is borrowing; the library reaches this function only from inside `inflateBack`, and the
    // harness holds no borrow across that call. `in()` and `out()` are never active at once.
    let bridge = unsafe { &mut *handle };
    if buf.is_null() || !buf.is_aligned() {
        // Tested after the bridge is in hand so that the source can be told, which is the only
        // way a harness could ever observe it. `zlib.h` L1170-L1172 lets a failing `in()` leave
        // `buf` untouched, so refusing is always available and nothing is written through it.
        bridge.source.note_unusable_out_parameter();
        return 0;
    }
    let Some(chunk) = bridge.source.next_chunk() else {
        return 0;
    };
    let Ok(count) = c_uint::try_from(chunk.len()) else {
        return 0;
    };
    if count == 0 {
        return 0;
    }

    // SAFETY: `buf` is a live, aligned out-parameter of exactly this type by this function's
    // contract, and `chunk` is valid for `count` readable bytes until the next call -- the
    // lifetime `'i` on [`BackSource`] is what guarantees it outlives this one.
    unsafe { buf.write(chunk.as_ptr()) };
    count
}

/// C's `out()`, bridged to [`BackSink`].
///
/// # Safety
///
/// `desc` must be null or a pointer to a live [`BackBridge`] nothing else is borrowing, and
/// `buf`/`len` must describe `len` readable bytes -- which is what `infback.c` reports.
unsafe extern "C" fn push(desc: *mut c_void, buf: *mut u8, len: c_uint) -> c_int {
    let handle = desc.cast::<BackBridge<'_, '_>>();
    if handle.is_null() || !handle.is_aligned() {
        // Mirrors `pull`'s null-descriptor branch on the success side: `infcover.c`'s `push`
        // treats a null descriptor as the non-failing case (its L467).
        return 0;
    }

    // SAFETY: as `pull`.
    let bridge = unsafe { &mut *handle };
    let count = len as usize;
    if buf.is_null() {
        // No slice is formed: `slice::from_raw_parts` is undefined for a null pointer even at
        // length zero. The sink is told the region lies outside the window, which is the only
        // honest thing that can be said about it, and the call is then refused -- a library that
        // reported a null region has already broken `zlib.h` L1136-L1137, and there is nothing
        // to accept.
        let _ = bridge.sink.write_out(&[], None);
        return 1;
    }

    // The offset is computed from the addresses rather than dereferencing anything, so a region
    // outside the window is reported instead of read.
    let base = bridge.window as usize;
    let start = buf as usize;
    let offset = start
        .checked_sub(base)
        .filter(|start| start.saturating_add(count) <= bridge.window_len);

    // SAFETY: `buf` is valid for `count` readable, initialised bytes for the duration of this
    // call: `infback.c` reports only what it has itself written into the window it was given,
    // and the window outlives the `inflateBack` call because the caller borrows it. The slice is
    // handed to safe code and not retained.
    let data = unsafe { core::slice::from_raw_parts(buf.cast_const(), count) };
    c_int::from(!bridge.sink.write_out(data, offset))
}

/// `inflateBackInit_(strm, windowBits, window, ZLIB_VERSION, size)`.
///
/// The window is the caller's, must be at least `1 << windowBits` bytes, and must stay
/// untouched until `inflateBack` returns -- `zlib.h` L1175-L1177.
#[must_use]
pub fn inflate_back_init(strm: &mut z_stream, window_bits: c_int, window: &mut [u8]) -> c_int {
    let ptr = if window.is_empty() {
        ptr::null_mut()
    } else {
        window.as_mut_ptr()
    };
    // SAFETY: obligations (a), (b), (d) and (e). `window` is exclusively borrowed and outlives
    // the initialisation; a zero-length window is offered as null, which is a documented
    // argument the entry point tests for before the null-stream test.
    unsafe {
        libz_rs_sys::inflateBackInit_(strm, window_bits, ptr, ZLIB_VERSION.as_ptr(), stream_size())
    }
}

/// `inflateBack(strm, in, in_desc, out, out_desc)` driven by a safe source and sink.
///
/// `window` must be the same slice [`inflate_back_init`] was given: it is what the offsets
/// handed to [`BackSink::write_out`] are computed against.
#[must_use]
pub fn inflate_back<'i, S, K>(
    strm: &mut z_stream,
    window: &[u8],
    source: &mut S,
    sink: &mut K,
) -> c_int
where
    S: BackSource<'i>,
    K: BackSink,
{
    let mut bridge = BackBridge {
        source,
        sink,
        window: window.as_ptr(),
        window_len: window.len(),
    };
    let desc = core::ptr::addr_of_mut!(bridge).cast::<c_void>();
    let in_hook: in_func = Some(pull);
    let out_hook: out_func = Some(push);
    // SAFETY: obligations (a) through (d), plus the two callbacks: both are `extern "C"` items
    // with exactly the declared signatures, and `desc` addresses `bridge`, which lives on this
    // stack frame for the whole call and is borrowed by nothing else -- the two trait objects
    // inside it are reached only from `pull`/`push`, which the library invokes one at a time
    // from inside this call.
    unsafe { libz_rs_sys::inflateBack(strm, in_hook, desc, out_hook, desc) }
}

/// `inflateBackEnd(strm)`.
#[must_use]
pub fn inflate_back_end(strm: &mut z_stream) -> c_int {
    // SAFETY: obligations (a) and (b), as [`inflate_end`].
    unsafe { libz_rs_sys::inflateBackEnd(strm) }
}

/// `inflateBackInit_(Z_NULL, windowBits, window, ZLIB_VERSION, size)` -- the null-stream refusal.
///
/// A gate of its own because obligation (a) makes a `&mut z_stream` non-null by construction, and
/// the refusal is part of the contract: `infback.c` L33-L35 answers `Z_STREAM_ERROR` once the
/// version handshake has passed. Nothing is installed, so nothing leaks and there is nothing to
/// end.
#[must_use]
pub fn inflate_back_init_null_stream(window_bits: c_int, window: &mut [u8]) -> c_int {
    // SAFETY: obligations (d) and (e). A null `z_streamp` is documented input, which the entry
    // point tests rather than dereferences; the window is exclusively borrowed and outlives the
    // call, and this side's own version and size are passed so the handshake is what succeeds.
    unsafe {
        libz_rs_sys::inflateBackInit_(
            ptr::null_mut(),
            window_bits,
            window.as_mut_ptr(),
            ZLIB_VERSION.as_ptr(),
            stream_size(),
        )
    }
}

/// `inflateBackInit_(Z_NULL, windowBits, window, Z_NULL, 0)` -- the version check, first.
///
/// ★ The ordering *is* the assertion, which is why this exists beside
/// [`inflate_back_init_null_stream`] and differs from it only in the last two arguments.
/// `infback.c` L30-L35 checks the version-and-layout handshake *before* it looks at the stream
/// pointer, so a null version with a zero size must answer `Z_VERSION_ERROR` even though the
/// stream is also null. An implementation that tested the pointer first would answer
/// `Z_STREAM_ERROR`, pass a naive "rejects bad input" test, and still be wrong.
#[must_use]
pub fn inflate_back_init_null_stream_null_version(window_bits: c_int, window: &mut [u8]) -> c_int {
    // SAFETY: obligation (d). Both the stream and the version pointer are null, and both are
    // arguments this entry point is contracted to test rather than dereference; the window is
    // exclusively borrowed and outlives the call.
    unsafe {
        libz_rs_sys::inflateBackInit_(
            ptr::null_mut(),
            window_bits,
            window.as_mut_ptr(),
            ptr::null(),
            0,
        )
    }
}

/// `inflateBack(Z_NULL, Z_NULL, Z_NULL, Z_NULL, Z_NULL)` -- the null-stream refusal.
///
/// `infback.c` L209-L210 tests the stream before anything else, so neither null callback is ever
/// invoked. That ordering is what makes passing them null safe rather than merely untested.
#[must_use]
pub fn inflate_back_null_stream() -> c_int {
    // SAFETY: every argument is a null the entry point tests rather than uses. The stream check
    // comes first and returns, so no null function pointer is called.
    unsafe {
        libz_rs_sys::inflateBack(
            ptr::null_mut(),
            None,
            ptr::null_mut(),
            None,
            ptr::null_mut(),
        )
    }
}

/// `inflateBackEnd(Z_NULL)` -- the null-stream refusal (`infback.c` L573).
#[must_use]
pub fn inflate_back_end_null_stream() -> c_int {
    // SAFETY: a null `z_streamp`, which this entry point is contracted to reject rather than
    // dereference.
    unsafe { libz_rs_sys::inflateBackEnd(ptr::null_mut()) }
}

// =================================================================================================
//  Checksums and introspection
// =================================================================================================

// The four buffer entry points come in two faces, and they are separate gates because C gives
// them separate answers. A live slice's own pointer -- non-null even when the slice is empty --
// is an ordinary update, so a zero-length call returns the incoming check value unchanged;
// `Z_NULL` in the same position is the documented request for the family's initial value
// (`zlib.h` L1812-L1814 and L1851-L1852), which discards both the incoming value and the
// length. `adler32.c` L81-L82 and `crc32.c` L627-L628 are where the reference makes the
// distinction. Collapsing an empty slice onto the null face would silently substitute `1` for
// `adler` and `0` for `crc`, so `data.as_ptr()` is passed verbatim here and the null face has
// its own four gates below.

/// `adler32(adler, buf, len)` over a live slice.
#[must_use]
pub fn adler32(adler: uLong, data: &[u8]) -> uLong {
    // SAFETY: obligation (c). `data.len()` bytes are readable at `data.as_ptr()` for the whole
    // of the call; `u8` has alignment 1 so the pointer is aligned, and it is non-null even for
    // an empty slice, which makes a zero-length read trivially in range. The callee only reads
    // through the pointer and retains nothing past its return.
    unsafe { libz_rs_sys::adler32(adler, data.as_ptr(), narrow_uint(data.len())) }
}

/// `adler32_z(adler, buf, len)` over a live slice, the `size_t` form.
#[must_use]
pub fn adler32_z(adler: uLong, data: &[u8]) -> uLong {
    // SAFETY: obligation (c), as [`adler32`]. No length conversion is involved: `z_size_t` is
    // `size_t`, which the facade asserts equal to `usize` at compile time.
    unsafe { libz_rs_sys::adler32_z(adler, data.as_ptr(), data.len()) }
}

/// `adler32(adler, Z_NULL, len)` -- the seed-request face.
///
/// The answer is `1` for every `len` and every `adler`. The port reaches the null test before
/// any read, so unlike the reference it is defined at `len == 1` too; see [`crate::oracle`]'s
/// counterpart, which documents why that one length cannot be asked of C.
#[must_use]
pub fn adler32_null(adler: uLong, len: uInt) -> uLong {
    // SAFETY: obligation (c) in its null form. A null buffer pointer is documented input here,
    // and `crates/libz-rs-sys/src/checksum.rs` tests `buf.is_null()` before it constructs any
    // slice, so nothing dereferences the pointer and no slice is ever formed from it.
    unsafe { libz_rs_sys::adler32(adler, ptr::null(), len) }
}

/// `adler32_z(adler, Z_NULL, len)` -- the seed-request face, the `size_t` form.
#[must_use]
pub fn adler32_z_null(adler: uLong, len: z_size_t) -> uLong {
    // SAFETY: as [`adler32_null`].
    unsafe { libz_rs_sys::adler32_z(adler, ptr::null(), len) }
}

/// `adler32_combine(adler1, adler2, len2)`.
#[must_use]
pub fn adler32_combine(adler1: uLong, adler2: uLong, len2: z_off_t) -> uLong {
    // No gate and no `unsafe`: three integers in, one out, so the facade declares this
    // `extern "C"` without `unsafe` and it is callable directly.
    libz_rs_sys::adler32_combine(adler1, adler2, len2)
}

/// `adler32_combine64(adler1, adler2, len2)`.
#[must_use]
pub fn adler32_combine64(adler1: uLong, adler2: uLong, len2: z_off64_t) -> uLong {
    // No gate and no `unsafe`, as [`adler32_combine`].
    libz_rs_sys::adler32_combine64(adler1, adler2, len2)
}

/// `crc32(crc, buf, len)` over a live slice.
#[must_use]
pub fn crc32(crc: uLong, data: &[u8]) -> uLong {
    // SAFETY: obligation (c), as [`adler32`].
    unsafe { libz_rs_sys::crc32(crc, data.as_ptr(), narrow_uint(data.len())) }
}

/// `crc32_z(crc, buf, len)` over a live slice, the `size_t` form.
#[must_use]
pub fn crc32_z(crc: uLong, data: &[u8]) -> uLong {
    // SAFETY: obligation (c), as [`adler32_z`].
    unsafe { libz_rs_sys::crc32_z(crc, data.as_ptr(), data.len()) }
}

/// `crc32(crc, Z_NULL, len)` -- the seed-request face.
///
/// The answer is `0` for every `len` and every `crc`.
#[must_use]
pub fn crc32_null(crc: uLong, len: uInt) -> uLong {
    // SAFETY: as [`adler32_null`].
    unsafe { libz_rs_sys::crc32(crc, ptr::null(), len) }
}

/// `crc32_z(crc, Z_NULL, len)` -- the seed-request face, the `size_t` form.
#[must_use]
pub fn crc32_z_null(crc: uLong, len: z_size_t) -> uLong {
    // SAFETY: as [`adler32_null`].
    unsafe { libz_rs_sys::crc32_z(crc, ptr::null(), len) }
}

/// `crc32_combine(crc1, crc2, len2)`.
#[must_use]
pub fn crc32_combine(crc1: uLong, crc2: uLong, len2: z_off_t) -> uLong {
    // No gate and no `unsafe`, as [`adler32_combine`].
    libz_rs_sys::crc32_combine(crc1, crc2, len2)
}

/// `crc32_combine64(crc1, crc2, len2)`.
#[must_use]
pub fn crc32_combine64(crc1: uLong, crc2: uLong, len2: z_off64_t) -> uLong {
    // No gate and no `unsafe`, as [`adler32_combine`].
    libz_rs_sys::crc32_combine64(crc1, crc2, len2)
}

/// `crc32_combine_gen(len2)`.
#[must_use]
pub fn crc32_combine_gen(len2: z_off_t) -> uLong {
    // No gate and no `unsafe`, as [`adler32_combine`].
    libz_rs_sys::crc32_combine_gen(len2)
}

/// `crc32_combine_gen64(len2)`.
#[must_use]
pub fn crc32_combine_gen64(len2: z_off64_t) -> uLong {
    // No gate and no `unsafe`, as [`adler32_combine`].
    libz_rs_sys::crc32_combine_gen64(len2)
}

/// `crc32_combine_op(crc1, crc2, op)`.
#[must_use]
pub fn crc32_combine_op(crc1: uLong, crc2: uLong, op: uLong) -> uLong {
    // No gate and no `unsafe`, as [`adler32_combine`].
    libz_rs_sys::crc32_combine_op(crc1, crc2, op)
}

/// `get_crc_table()`, as the 256-entry table it points at.
///
/// The C contract is a pointer to `static const` data of at least 256 entries -- `crc32.c`
/// L216-L232 -- which is what makes a `'static` slice the honest Rust shape.
#[must_use]
pub fn crc_table() -> &'static [z_crc_t] {
    // SAFETY: the returned pointer addresses the library's own `const`-evaluated table, which
    // has static storage duration and at least 256 elements of exactly this type; nothing ever
    // writes to it, so a shared `'static` slice is sound and the length is the documented one.
    unsafe { core::slice::from_raw_parts(libz_rs_sys::get_crc_table(), 256) }
}

/// `zlibVersion()`, as the string it points at.
#[must_use]
pub fn zlib_version() -> &'static CStr {
    // SAFETY: obligation (g). The pointer addresses a NUL-terminated string literal with static
    // storage duration compiled into the library, so it is valid for reads for `'static` and
    // never written.
    unsafe { CStr::from_ptr(libz_rs_sys::zlibVersion()) }
}

/// `zlibCompileFlags()`.
#[must_use]
pub fn zlib_compile_flags() -> uLong {
    libz_rs_sys::zlibCompileFlags()
}

/// `zError(err)`, as the message it points at.
#[must_use]
pub fn z_error(err: c_int) -> &'static CStr {
    // SAFETY: obligation (g), as [`zlib_version`]: the message table is `'static` const data.
    unsafe { CStr::from_ptr(libz_rs_sys::zError(err)) }
}

// =================================================================================================
//  The gzip file layer
// =================================================================================================

/// A `gzFile` handle, and the only way this crate's consumers obtain one.
///
/// `gzFile` is an opaque `*mut gzFile_s`, so a free function taking one would be a safe
/// signature over an arbitrary pointer. Wrapping it means a handle can only have come from
/// [`GzFile::open`], [`GzFile::open64`] or [`GzFile::dopen`], which is obligation (g)'s first
/// half; the second half -- exactly one `close` -- stays the caller's, and is why this type is
/// `Copy` rather than owning: `gzclose` consumes the handle in C, and the harness deliberately
/// exercises the double-close and use-after-close *diagnostics* the library returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GzFile(gzFile);

impl GzFile {
    /// `gzopen(path, mode)`.
    #[must_use]
    pub fn open(path: &CStr, mode: &CStr) -> Self {
        // SAFETY: obligation (g). Both arguments are NUL-terminated and outlive the call, and
        // the library copies the path it keeps (`gzlib.c` L200-L204).
        Self(unsafe { libz_rs_sys::gzopen(path.as_ptr(), mode.as_ptr()) })
    }

    /// `gzopen64(path, mode)`.
    #[must_use]
    pub fn open64(path: &CStr, mode: &CStr) -> Self {
        // SAFETY: obligation (g), as [`GzFile::open`].
        Self(unsafe { libz_rs_sys::gzopen64(path.as_ptr(), mode.as_ptr()) })
    }

    /// `gzdopen(fd, mode)`.
    ///
    /// The handle takes ownership of `fd` and `gzclose` closes it (`zlib.h` L1412-L1419), so the
    /// caller must not use the descriptor again.
    #[must_use]
    pub fn dopen(fd: c_int, mode: &CStr) -> Self {
        // SAFETY: obligation (g) for the mode string; `fd` is a descriptor the caller has just
        // obtained and hands over, so there is exactly one owner at every moment.
        Self(unsafe { libz_rs_sys::gzdopen(fd, mode.as_ptr()) })
    }

    /// `gzopen(Z_NULL, mode)` -- the refusal `gzlib.c` L96-L97 owes a null path.
    ///
    /// A separate gate because a null `*const c_char` cannot be spelled as a `&CStr`, and the
    /// answer is part of the contract: the result must be `Z_NULL`, so there is nothing to close.
    #[must_use]
    pub fn open_null_path(mode: &CStr) -> Self {
        // SAFETY: obligation (g) for the mode string. A null path is documented input, and
        // `gz_open` tests it before any dereference, so nothing reads through the null.
        Self(unsafe { libz_rs_sys::gzopen(ptr::null(), mode.as_ptr()) })
    }

    /// `gzopen(path, Z_NULL)` -- the refusal a null mode string owes.
    #[must_use]
    pub fn open_null_mode(path: &CStr) -> Self {
        // SAFETY: as [`GzFile::open_null_path`], with the null in the other position.
        Self(unsafe { libz_rs_sys::gzopen(path.as_ptr(), ptr::null()) })
    }

    /// `gzopen64(Z_NULL, mode)` -- the large-file spelling of [`GzFile::open_null_path`].
    #[must_use]
    pub fn open64_null_path(mode: &CStr) -> Self {
        // SAFETY: as [`GzFile::open_null_path`]; `gzopen64` forwards to the same `gz_open`.
        Self(unsafe { libz_rs_sys::gzopen64(ptr::null(), mode.as_ptr()) })
    }

    /// The `Z_NULL` handle, which every `gz*` entry point is documented to tolerate.
    ///
    /// `zlib.h` gives each of them an answer for it -- `0` for `gzeof` and `gzdirect`, `-1` for
    /// most of the rest, `Z_STREAM_ERROR` for the close and parameter forms, `NULL` for `gzgets`
    /// and `gzerror` -- and every one of those answers comes from a guard at the head of the
    /// function, before anything is dereferenced. So a null handle may be driven through the same
    /// gates a live one is, and there is nothing to close afterwards.
    #[must_use]
    pub fn null() -> Self {
        Self(ptr::null_mut())
    }

    /// Whether the open failed, which C reports as a null handle.
    #[must_use]
    pub fn is_null(self) -> bool {
        self.0.is_null()
    }

    /// The three members of the exposed `struct gzFile_s` prefix, or [`None`] for a null handle.
    ///
    /// `zlib.h` L1966-L1968 defines `gzgetc(g)` as a macro that reads and mutates `have`, `next`
    /// and `pos` *inside caller-compiled object code*, so the prefix is part of the ABI and the
    /// harness asserts on it. `next` is reported as a `bool` -- whether it is non-null -- because
    /// the pointer's value is not a property any test can compare between implementations.
    #[must_use]
    pub fn prefix(self) -> Option<(c_uint, bool, z_off64_t)> {
        if self.0.is_null() || !self.0.is_aligned() {
            return None;
        }
        // SAFETY: `self.0` is a live, aligned handle this module opened and the caller has not
        // closed, and `gzFile_s` is the `#[repr(C)]` mirror of the prefix `gzguts.h` L170-L175
        // embeds as `gz_state`'s first member -- so these three reads are of initialised members
        // of exactly the declared types. Nothing is written and no reference escapes.
        let (have, next, pos) =
            unsafe { ((*self.0).have, (*self.0).next.is_null(), (*self.0).pos) };
        Some((have, !next, pos))
    }

    /// Consumes one byte exactly as the `gzgetc` **macro** does, or [`None`] when it defers.
    ///
    /// `zlib.h` L1966-L1968 expands `gzgetc(g)` to
    /// `((g)->have ? ((g)->have--, (g)->pos++, *((g)->next)++) : (gzgetc)(g))` inside the
    /// **caller's** object code. So this is not poking at an internal: it is what every program
    /// that uses `gzgetc` genuinely does to the state, the library has to tolerate it and
    /// recover its own cursor on the next call, and no amount of encapsulation on this side can
    /// change that. Reproducing the macro is the only way to hold the port to it.
    ///
    /// [`None`] is the macro's other branch -- `have == 0`, where it defers to the function --
    /// so a caller that wants the whole macro falls back to [`GzFile::getc`] on `None`.
    ///
    /// Nothing past the 24-byte prefix is touched. Everything after it is the port's private
    /// state and is out of contract.
    #[must_use]
    pub fn macro_take(self) -> Option<u8> {
        if self.0.is_null() || !self.0.is_aligned() {
            return None;
        }
        // SAFETY: as [`GzFile::prefix`] -- the handle is live and aligned and the three members
        // are the `#[repr(C)]` prefix at offset 0.
        let (have, next, pos) = unsafe { ((*self.0).have, (*self.0).next, (*self.0).pos) };
        if have == 0 || next.is_null() {
            return None;
        }
        // SAFETY: `have` is non-zero, which is the library's own statement that `have` bytes are
        // readable at `next` (`gzguts.h` L173), so one of them is; and `next.add(1)` is in bounds
        // because `have >= 1` puts at least one byte plus the one-past-the-end position inside
        // the library's output buffer. The three writes together are the macro's own single-byte
        // step, so they leave the prefix coherent -- the cursor advances by exactly the amount
        // `have` shrinks by -- which is what the port's resynchronisation requires.
        let byte = unsafe {
            let byte = *next;
            (*self.0).have = have - 1;
            (*self.0).next = next.add(1);
            (*self.0).pos = pos.wrapping_add(1);
            byte
        };
        Some(byte)
    }

    /// `gzbuffer(file, size)`.
    #[must_use]
    pub fn buffer(self, size: c_uint) -> c_int {
        // SAFETY: obligation (g). `file` is either a handle this module opened and the caller has
        // not closed, or the documented `Z_NULL` of [`GzFile::null`], which every entry point
        // answers from a guard before any dereference; the size is a plain integer. Every gate
        // below relies on the same clause.
        unsafe { libz_rs_sys::gzbuffer(self.0, size) }
    }

    /// `gzsetparams(file, level, strategy)`.
    #[must_use]
    pub fn setparams(self, level: c_int, strategy: c_int) -> c_int {
        // SAFETY: obligation (g), as [`GzFile::buffer`].
        unsafe { libz_rs_sys::gzsetparams(self.0, level, strategy) }
    }

    /// `gzwrite(file, buf, len)`.
    #[must_use]
    pub fn write(self, data: &[u8]) -> c_int {
        // SAFETY: obligations (c) and (g). `len` bytes really are readable at that pointer,
        // both taken from one live slice, and the library copies them into its own buffer
        // rather than retaining the pointer.
        unsafe { libz_rs_sys::gzwrite(self.0, data.as_ptr().cast(), narrow_uint(data.len())) }
    }

    /// `gzfwrite(buf, 1, nitems, file)`.
    #[must_use]
    pub fn fwrite(self, data: &[u8]) -> z_size_t {
        // SAFETY: obligations (c) and (g). `size` is 1 and `nitems` the length, so the product
        // cannot overflow and the readable region is exactly `data`.
        unsafe { libz_rs_sys::gzfwrite(data.as_ptr().cast(), 1, data.len(), self.0) }
    }

    /// `gzfwrite(buf, size, nitems, file)` with the two factors given separately.
    ///
    /// The product `size * nitems` is what the library reads, and `zlib.h` L1533-L1539 makes an
    /// overflowing product a documented error rather than undefined behaviour -- so the harness
    /// needs to be able to present the factors it chooses.
    #[must_use]
    pub fn fwrite_items(self, data: &[u8], size: z_size_t, nitems: z_size_t) -> z_size_t {
        // The extra clause the caller owes here, checked rather than assumed: `size * nitems`
        // must not exceed `data.len()`. When it would, the call is not made and zero is
        // reported, exactly as the library reports for a request it refuses.
        match size.checked_mul(nitems) {
            // SAFETY: obligations (c) and (g), plus the product clause established by the
            // guard on this arm: `size * nitems` readable bytes really are there.
            Some(total) if total <= data.len() => unsafe {
                libz_rs_sys::gzfwrite(data.as_ptr().cast(), size, nitems, self.0)
            },
            _ => 0,
        }
    }

    /// `gzputc(file, c)`.
    #[must_use]
    pub fn putc(self, byte: c_int) -> c_int {
        // SAFETY: obligation (g), as [`GzFile::buffer`].
        unsafe { libz_rs_sys::gzputc(self.0, byte) }
    }

    /// `gzputs(file, s)`.
    #[must_use]
    pub fn puts(self, text: &CStr) -> c_int {
        // SAFETY: obligations (g) and, for `text`, that it is NUL-terminated, read-only for the
        // library and outlives the call.
        unsafe { libz_rs_sys::gzputs(self.0, text.as_ptr()) }
    }

    /// `gzflush(file, flush)`.
    #[must_use]
    pub fn flush(self, flush: c_int) -> c_int {
        // SAFETY: obligation (g), as [`GzFile::buffer`].
        unsafe { libz_rs_sys::gzflush(self.0, flush) }
    }

    /// `gzread(file, buf, len)`.
    #[must_use]
    pub fn read(self, buf: &mut [u8]) -> c_int {
        // SAFETY: obligations (d) and (g). `len` bytes really are writable at that pointer, both
        // taken from one live exclusive slice.
        unsafe { libz_rs_sys::gzread(self.0, buf.as_mut_ptr().cast(), narrow_uint(buf.len())) }
    }

    /// `gzfread(buf, 1, nitems, file)`.
    #[must_use]
    pub fn fread(self, buf: &mut [u8]) -> z_size_t {
        // SAFETY: obligations (d) and (g), with `size` 1 and `nitems` the length.
        unsafe { libz_rs_sys::gzfread(buf.as_mut_ptr().cast(), 1, buf.len(), self.0) }
    }

    /// `gzfread(buf, size, nitems, file)` with the two factors given separately.
    #[must_use]
    pub fn fread_items(self, buf: &mut [u8], size: z_size_t, nitems: z_size_t) -> z_size_t {
        // The product clause [`GzFile::fwrite_items`] records, applied to the destination.
        match size.checked_mul(nitems) {
            // SAFETY: obligations (d) and (g), plus the product clause established by the
            // guard on this arm: `size * nitems` writable bytes really are there.
            Some(total) if total <= buf.len() => unsafe {
                libz_rs_sys::gzfread(buf.as_mut_ptr().cast(), size, nitems, self.0)
            },
            _ => 0,
        }
    }

    /// `gzgets(file, buf, len)`, as the bytes written before the terminating NUL.
    ///
    /// [`None`] is C's null return: end of input, or an error.
    ///
    /// # Panics
    ///
    /// Panics if the call answers with a pointer that is neither the caller's own buffer nor
    /// null. `zlib.h` L1555-L1563 and `gzread.c` L620-L623 admit only those two, and this is the
    /// one place the returned pointer is still in scope to be compared, so the check is made
    /// here rather than left to a caller that no longer has it.
    #[must_use]
    pub fn gets(self, buf: &mut [u8]) -> Option<Vec<u8>> {
        let expected: *mut c_char = buf.as_mut_ptr().cast();
        // SAFETY: obligations (d) and (g). `gzgets` writes at most `len` bytes including the
        // terminating NUL (`zlib.h` L1555-L1563), and `len` here is exactly the slice's length.
        let answer = unsafe { libz_rs_sys::gzgets(self.0, expected, narrow_int(buf.len())) };
        assert!(
            answer == expected || answer.is_null(),
            "gzgets must answer with the caller's own buffer or NULL, nothing else"
        );
        if answer.is_null() {
            return None;
        }
        // SAFETY: on a non-null return the library has written a NUL-terminated string into
        // `buf`, which is still borrowed and therefore still live, and `answer` addresses its
        // first byte.
        let text = unsafe { CStr::from_ptr(answer) };
        Some(text.to_bytes().to_vec())
    }

    /// `gzgetc(file)` -- the function, not the macro.
    #[must_use]
    pub fn getc(self) -> c_int {
        // SAFETY: obligation (g), as [`GzFile::buffer`].
        unsafe { libz_rs_sys::gzgetc(self.0) }
    }

    /// `gzgetc_(file)`, the always-a-function form the macro falls back to.
    #[must_use]
    pub fn getc_(self) -> c_int {
        // SAFETY: obligation (g), as [`GzFile::buffer`].
        unsafe { libz_rs_sys::gzgetc_(self.0) }
    }

    /// `gzungetc(c, file)`. Note C's argument order, which puts the byte first.
    #[must_use]
    pub fn ungetc(self, byte: c_int) -> c_int {
        // SAFETY: obligation (g), as [`GzFile::buffer`].
        unsafe { libz_rs_sys::gzungetc(byte, self.0) }
    }

    /// `gzseek(file, offset, whence)`.
    #[must_use]
    pub fn seek(self, offset: z_off_t, whence: c_int) -> z_off_t {
        // SAFETY: obligation (g), as [`GzFile::buffer`].
        unsafe { libz_rs_sys::gzseek(self.0, offset, whence) }
    }

    /// `gzseek64(file, offset, whence)`.
    #[must_use]
    pub fn seek64(self, offset: z_off64_t, whence: c_int) -> z_off64_t {
        // SAFETY: obligation (g), as [`GzFile::buffer`].
        unsafe { libz_rs_sys::gzseek64(self.0, offset, whence) }
    }

    /// `gzrewind(file)`.
    #[must_use]
    pub fn rewind(self) -> c_int {
        // SAFETY: obligation (g), as [`GzFile::buffer`].
        unsafe { libz_rs_sys::gzrewind(self.0) }
    }

    /// `gztell(file)`.
    #[must_use]
    pub fn tell(self) -> z_off_t {
        // SAFETY: obligation (g), as [`GzFile::buffer`].
        unsafe { libz_rs_sys::gztell(self.0) }
    }

    /// `gztell64(file)`.
    #[must_use]
    pub fn tell64(self) -> z_off64_t {
        // SAFETY: obligation (g), as [`GzFile::buffer`].
        unsafe { libz_rs_sys::gztell64(self.0) }
    }

    /// `gzoffset(file)`.
    #[must_use]
    pub fn offset(self) -> z_off_t {
        // SAFETY: obligation (g), as [`GzFile::buffer`].
        unsafe { libz_rs_sys::gzoffset(self.0) }
    }

    /// `gzoffset64(file)`.
    #[must_use]
    pub fn offset64(self) -> z_off64_t {
        // SAFETY: obligation (g), as [`GzFile::buffer`].
        unsafe { libz_rs_sys::gzoffset64(self.0) }
    }

    /// `gzeof(file)`.
    #[must_use]
    pub fn eof(self) -> c_int {
        // SAFETY: obligation (g), as [`GzFile::buffer`].
        unsafe { libz_rs_sys::gzeof(self.0) }
    }

    /// `gzdirect(file)`, which distinguishes a decompressed stream from a transparent copy.
    #[must_use]
    pub fn direct(self) -> c_int {
        // SAFETY: obligation (g), as [`GzFile::buffer`].
        unsafe { libz_rs_sys::gzdirect(self.0) }
    }

    /// `gzerror(file, &errnum)`, as the code and the message together.
    ///
    /// `sentinel` is what `errnum` is initialised to, so a caller can tell "the library wrote
    /// nothing" from "the library wrote zero". The message is [`None`] for the null return and
    /// `Some` for every other, including `Some(&[])` for a live handle with no error: `gzlib.c`
    /// L517 answers null only for a null or wrongly-moded handle, whereas L527 answers `""`, and
    /// a comparison that collapsed the two would not see a side that swapped them.
    #[must_use]
    pub fn error(self, sentinel: c_int) -> (c_int, Option<Vec<u8>>) {
        let mut errnum: c_int = sentinel;
        // SAFETY: obligations (f) and (g). `errnum` is a live local of exactly the declared
        // type, initialised before the call and outliving it, and the pointer is not retained;
        // `addr_of_mut!` rather than `&mut` because C writes through it.
        let message = unsafe { libz_rs_sys::gzerror(self.0, core::ptr::addr_of_mut!(errnum)) };
        (errnum, gz_message(message))
    }

    /// `gzerror(file, NULL)` -- the form that asks only for the message.
    ///
    /// `gzlib.c` L521-L522 makes the out-parameter optional, and nothing exercises that half of
    /// the contract except a caller that passes null deliberately. Returns whether a message
    /// came back, which is the only thing this form can report.
    #[must_use]
    pub fn error_message_only(self) -> bool {
        // SAFETY: obligation (g). A null `errnum` is explicitly permitted and is not
        // dereferenced by the callee.
        let message = unsafe { libz_rs_sys::gzerror(self.0, ptr::null_mut()) };
        !message.is_null()
    }

    /// `gzclearerr(file)`.
    pub fn clearerr(self) {
        // SAFETY: obligation (g), as [`GzFile::buffer`].
        unsafe { libz_rs_sys::gzclearerr(self.0) }
    }

    /// `gzclose(file)`. Consumes the handle: no method may be called on it afterwards.
    #[must_use]
    pub fn close(self) -> c_int {
        // SAFETY: obligation (g)'s exactly-one-`gzclose` half, which the caller owes and which
        // taking `self` by value expresses as far as a `Copy` handle can.
        unsafe { libz_rs_sys::gzclose(self.0) }
    }

    /// `gzclose_r(file)`, the read-side form.
    #[must_use]
    pub fn close_r(self) -> c_int {
        // SAFETY: as [`GzFile::close`].
        unsafe { libz_rs_sys::gzclose_r(self.0) }
    }

    /// `gzclose_w(file)`, the write-side form.
    #[must_use]
    pub fn close_w(self) -> c_int {
        // SAFETY: as [`GzFile::close`].
        unsafe { libz_rs_sys::gzclose_w(self.0) }
    }
}

/// The bytes of a `gzerror` message, or [`None`] for a null pointer.
fn gz_message(message: *const c_char) -> Option<Vec<u8>> {
    if message.is_null() {
        return None;
    }
    // SAFETY: `gzerror` returns either null -- handled above -- or a NUL-terminated string owned
    // by the stream, valid for reads until the next call on that handle. It is copied here
    // rather than borrowed, so nothing outlives that window.
    Some(unsafe { CStr::from_ptr(message) }.to_bytes().to_vec())
}

// =================================================================================================
//  The instrumented allocator
// =================================================================================================

/// The alignment a tracked block is allocated with.
///
/// `malloc`-grade: the library is entitled to it, and the reference implementation's own
/// allocations have it. 16 is the widest fundamental alignment on either Tier-1 64-bit target.
///
/// Not a free choice, and getting it wrong fails quietly rather than loudly. `zutil.c`'s
/// `zcalloc` and `test/infcover.c` L84 are both plain `malloc`, and the facade holds a caller's
/// allocator to the same standard: its `reserve::<T>` hands a misaligned block straight back and
/// answers `Z_MEM_ERROR`. An under-aligned hook would therefore not crash -- it would make
/// *every* stream fail to initialise, and every caller of this allocator would go on reporting a
/// clean run while testing nothing.
const HOOK_ALIGN: usize = 16;

/// The byte every tracked block is filled with before it is handed over.
///
/// `test/infcover.c` L46's choice, and the whole point of the instrumentation: a block is never
/// zeroed, so any code path that depends on zeroed memory misbehaves visibly instead of passing
/// by luck.
///
/// ★ MEASURED, through `fuzz/fuzz_targets/fuzz_inflate.rs`: substituting `0x00` here changes
/// nothing. A 60-second session with a zero fill completed 1,148,357 executions with the same
/// coverage and no failure, so this port has no dependence on zero-initialised memory to expose.
/// That is a result rather than an absence of evidence, and it has a mechanical explanation: the
/// facade fills every *buffer* allocation with its own `0xA5` before handing it to the core, and
/// the one allocation it deliberately does not pre-fill -- the state block -- has every byte
/// written by the move that occupies it before anything reads it, so the port cannot observe this
/// byte at all. The fill is kept regardless: it is what makes that argument checkable rather than
/// merely stated, and it would catch a future change that started handing a buffer out unfilled.
const SENTINEL_FILL: u8 = 0xa5;

/// One tracked block, in a singly-linked list newest-first -- C's `mem_item`
/// (`test/infcover.c` L56-L60).
struct MemItem {
    /// The block handed to the library.
    ptr: *mut u8,

    /// The size that was asked for, which is what the byte accounting uses.
    size: usize,

    /// The next-oldest block.
    next: Option<Box<MemItem>>,
}

/// The ledger -- C's `mem_zone` (`test/infcover.c` L62-L69).
struct MemZone {
    /// Live blocks, newest first.
    first: Option<Box<MemItem>>,

    /// Live bytes.
    total: usize,

    /// The largest value [`Self::total`] ever reached: `mem_high()`'s number.
    highwater: usize,

    /// Refuse any request that would push [`Self::total`] past this. Zero means no ceiling,
    /// exactly as `mem_limit()` defines it.
    limit: usize,

    /// Satisfied allocations.
    allocations: usize,

    /// Frees of a block the zone recognised.
    frees: usize,

    /// Requests refused by the ceiling or by an allocator failure.
    refusals: usize,

    /// Frees that were not of the most recent block -- C's `notlifo`.
    notlifo: usize,

    /// Frees of an address the zone never handed out -- C's `rogue`.
    rogue: usize,
}

/// What a [`TrackingAllocator`] observed. Every fault field must be zero for a clean run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrackingReport {
    /// Satisfied allocations.
    pub allocations: usize,

    /// Frees of a recognised block.
    pub frees: usize,

    /// Requests refused, by the ceiling or by the allocator itself.
    pub refusals: usize,

    /// Bytes still live. Non-zero after teardown is a leak.
    pub live_bytes: usize,

    /// Blocks still live. Non-zero after teardown is a leak.
    pub live_blocks: usize,

    /// The largest live-byte total ever reached -- `mem_high()`'s number, and what the
    /// per-stream memory comparison is made of.
    pub high_water: usize,

    /// Frees that were not of the most recent block.
    pub notlifo: usize,

    /// Frees of an address never handed out.
    pub rogue: usize,

    /// The ceiling this allocator was created with; zero means it had none.
    pub limit: usize,
}

impl TrackingReport {
    /// Whether the run was clean: nothing leaked, every free matched, in order, and no refusal
    /// went unaccounted for.
    ///
    /// The high-water invariants are part of it, so that a defect in the instrumentation cannot
    /// silently disable it: a mark below the live total, or one past a ceiling that was supposed
    /// to refuse, means the induced-failure logic is not working.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.live_bytes == 0
            && self.live_blocks == 0
            && self.notlifo == 0
            && self.rogue == 0
            && self.allocations == self.frees
            && self.high_water >= self.live_bytes
            && (self.limit == 0 || self.high_water <= self.limit)
    }
}

/// The layout a tracked block of `len` bytes is allocated with.
///
/// `len.max(1)` because Rust's allocator forbids a zero-sized request while C's `malloc(0)` is
/// legal; the zone still records the requested `len`, so the byte accounting matches C's.
/// Returns [`None`] rather than panicking if the pair is not a valid layout, which is what keeps
/// the hooks panic-free.
fn tracked_layout(len: usize) -> Option<Layout> {
    Layout::from_size_align(len.max(1), HOOK_ALIGN).ok()
}

/// C's `mem_alloc` (`test/infcover.c` L71-L109).
///
/// # Safety
///
/// `opaque` must be null or a pointer to a live [`MemZone`] that nothing else is borrowing for
/// the duration of the call. Installing it through [`TrackingAllocator::install`] and letting
/// only the library call this satisfies both.
unsafe extern "C" fn mem_alloc(opaque: voidpf, count: uInt, size: uInt) -> voidpf {
    let zone = opaque.cast::<MemZone>();
    if zone.is_null() || !zone.is_aligned() {
        // C's "induced allocation failure" for a missing zone (its L79-L80).
        return ptr::null_mut();
    }

    // SAFETY: the caller guarantees `opaque` addresses a live, aligned `MemZone` that nothing
    // else is borrowing while this call runs. The library reaches this function only from inside
    // an entry point, and the harness never holds a borrow across such a call.
    let zone = unsafe { &mut *zone };

    // `len = count * (size_t)size` (its L76), checked rather than assumed so the hook is
    // correct on any target and cannot panic on one where the product could overflow.
    let Some(len) = (count as usize).checked_mul(size as usize) else {
        zone.refusals = zone.refusals.saturating_add(1);
        return ptr::null_mut();
    };

    // The induced failure proper (its L79): `zone->limit && zone->total + len > zone->limit`.
    if zone.limit != 0 && zone.total.saturating_add(len) > zone.limit {
        zone.refusals = zone.refusals.saturating_add(1);
        return ptr::null_mut();
    }

    let Some(layout) = tracked_layout(len) else {
        zone.refusals = zone.refusals.saturating_add(1);
        return ptr::null_mut();
    };

    // SAFETY: `tracked_layout` never returns a zero-sized layout, which is `alloc`'s one
    // precondition.
    let block = unsafe { alloc(layout) };
    if block.is_null() {
        // C's `if (ptr == NULL) return NULL;` (its L85-L86).
        zone.refusals = zone.refusals.saturating_add(1);
        return ptr::null_mut();
    }

    // SAFETY: `alloc` just returned `block` as a valid, writable, uninitialised region of
    // exactly `layout.size()` bytes that nothing aliases yet. This is the 0xa5 fill of C's L87.
    unsafe { ptr::write_bytes(block, SENTINEL_FILL, layout.size()) };

    // Insert at the head of the list (its L98-L100).
    zone.first = Some(Box::new(MemItem {
        ptr: block,
        size: len,
        next: zone.first.take(),
    }));

    // Statistics (its L103-L105).
    zone.total = zone.total.saturating_add(len);
    zone.highwater = zone.highwater.max(zone.total);
    zone.allocations = zone.allocations.saturating_add(1);

    block.cast()
}

/// C's `mem_free` (`test/infcover.c` L112-L154).
///
/// One deliberate divergence: where C ends with an unconditional `free(ptr)`, this releases only
/// blocks the zone recognises. Returning a pointer of unknown provenance and unknown layout to
/// Rust's allocator would be undefined behaviour, so a rogue free is counted and otherwise
/// ignored -- and counting it is what matters, because the teardown assertion fails either way.
///
/// # Safety
///
/// As [`mem_alloc`]: `opaque` must be null or a live, unaliased [`MemZone`].
unsafe extern "C" fn mem_free(opaque: voidpf, address: voidpf) {
    let zone = opaque.cast::<MemZone>();
    if zone.is_null() || !zone.is_aligned() {
        // C's "if no zone, just do a free" (its L118-L121). Without a zone nothing was ever
        // handed out, so there is nothing to release.
        return;
    }

    // SAFETY: as [`mem_alloc`].
    let zone = unsafe { &mut *zone };
    let target = address.cast::<u8>();

    // Walk the list looking for `target`, unlinking it if found (its L125-L140). `hops` counts
    // how far in it was: zero is the head, the last-in-first-out case C treats as normal.
    let mut hops = 0usize;
    let mut cursor = &mut zone.first;
    let removed = loop {
        let found = match cursor.as_deref() {
            None => break None,
            Some(item) => ptr::eq(item.ptr, target),
        };
        if found {
            let Some(mut item) = cursor.take() else {
                break None;
            };
            *cursor = item.next.take();
            break Some(item);
        }
        let Some(item) = cursor.as_deref_mut() else {
            break None;
        };
        cursor = &mut item.next;
        hops = hops.saturating_add(1);
    };

    match removed {
        // "if not found, update the rogue count" (its L149-L150).
        None => zone.rogue = zone.rogue.saturating_add(1),
        Some(item) => {
            if hops != 0 {
                // "not a LIFO free" (its L136).
                zone.notlifo = zone.notlifo.saturating_add(1);
            }
            // "update the statistics and free the item" (its L143-L146).
            zone.total = zone.total.saturating_sub(item.size);
            zone.frees = zone.frees.saturating_add(1);
            if let Some(layout) = tracked_layout(item.size) {
                // SAFETY: `item.ptr` came from `alloc` in `mem_alloc` under
                // `tracked_layout(item.size)`, and `tracked_layout` is a pure function of
                // `size`, so the layout rebuilt here is the identical one. The node has just
                // been unlinked, so the block is released exactly once and nothing holds a
                // reference to it.
                unsafe { dealloc(item.ptr, layout) };
            }
        }
    }
}

/// The instrumented allocator both implementations are driven through, and the one piece of the
/// harness that is neither side's.
///
/// It is `test/infcover.c`'s `mem_zone` ported once: the `0xa5` fill that exposes any dependence
/// on zeroed memory, the leak, non-LIFO and rogue-free detection that validates the allocator
/// half of the boundary contract -- memory obtained from a caller's `zalloc` is released, in
/// order, only through that same caller's `zfree` -- the `mem_high()` high-water mark the
/// per-stream memory comparison is made of, and `mem_limit()`'s ceiling, which forces
/// `Z_MEM_ERROR` at controlled points and so reaches error paths fuzzing finds only by chance.
///
/// **The ledger cannot live in a local.** The pointer installed in `z_stream.opaque` is what the
/// two hooks dereference; a later write through a *local* zone would be a write through the
/// local's own provenance and would invalidate every pointer derived from it. Heap-allocating
/// once and routing every access -- the harness's through [`Self::report`], the library's
/// through `opaque` -- through this single pointer keeps the provenance chain intact. It is also
/// how `test/infcover.c` holds its own zone.
pub struct TrackingAllocator(*mut MemZone);

impl TrackingAllocator {
    /// An allocator with no ceiling.
    #[must_use]
    pub fn new() -> Self {
        Self::with_limit(0)
    }

    /// An allocator that refuses any request which would push the live total past `limit`.
    ///
    /// Zero means no ceiling, exactly as `mem_limit()` defines it.
    #[must_use]
    pub fn with_limit(limit: usize) -> Self {
        Self(Box::into_raw(Box::new(MemZone {
            first: None,
            total: 0,
            highwater: 0,
            limit,
            allocations: 0,
            frees: 0,
            refusals: 0,
            notlifo: 0,
            rogue: 0,
        })))
    }

    /// Points `strm` at this allocator, as `mem_setup` does (`test/infcover.c` L158-L173).
    pub fn install(&self, strm: &mut z_stream) {
        strm.zalloc = Some(mem_alloc);
        strm.zfree = Some(mem_free);
        strm.opaque = self.opaque();
    }

    /// The `zalloc` hook, for a caller that installs the three members itself.
    #[must_use]
    pub fn alloc_hook() -> libz_rs_sys::alloc_func {
        Some(mem_alloc)
    }

    /// The `zfree` hook, for a caller that installs the three members itself.
    #[must_use]
    pub fn free_hook() -> libz_rs_sys::free_func {
        Some(mem_free)
    }

    /// The pointer to install as `z_stream.opaque`.
    #[must_use]
    pub fn opaque(&self) -> voidpf {
        self.0.cast::<c_void>()
    }

    /// Borrows the ledger through its one raw pointer for the duration of `body`.
    fn with<R>(&self, body: impl FnOnce(&mut MemZone) -> R) -> R {
        // SAFETY: the pointer came from `Box::into_raw` in `with_limit`, is live until `drop`,
        // and is aligned and unique. No other reference exists while `body` runs: the library
        // forms one only inside `mem_alloc`/`mem_free`, neither of which can be executing,
        // because this call and any library call are on the same thread and neither is
        // re-entrant.
        body(unsafe { &mut *self.0 })
    }

    /// What the ledger holds right now, without disturbing it.
    #[must_use]
    pub fn report(&self) -> TrackingReport {
        self.with(|zone| {
            let mut live_blocks = 0usize;
            let mut cursor = zone.first.as_deref();
            while let Some(item) = cursor {
                live_blocks = live_blocks.saturating_add(1);
                cursor = item.next.as_deref();
            }
            TrackingReport {
                allocations: zone.allocations,
                frees: zone.frees,
                refusals: zone.refusals,
                live_bytes: zone.total,
                live_blocks,
                high_water: zone.highwater,
                notlifo: zone.notlifo,
                rogue: zone.rogue,
                limit: zone.limit,
            }
        })
    }

    /// Live bytes right now.
    #[must_use]
    pub fn live_bytes(&self) -> usize {
        self.with(|zone| zone.total)
    }

    /// The high-water mark, which is `mem_high()`'s number.
    #[must_use]
    pub fn high_water(&self) -> usize {
        self.with(|zone| zone.highwater)
    }

    /// C's `mem_done` (`test/infcover.c` L200-L234): release anything left behind, then report.
    ///
    /// The report's `live_blocks` and `live_bytes` are what was found *before* the release, so a
    /// leak is still visible after the leak has been cleaned up.
    #[must_use]
    pub fn finish(&self) -> TrackingReport {
        let report = self.report();
        self.with(MemZone::release);
        report
    }
}

impl Default for TrackingAllocator {
    fn default() -> Self {
        Self::new()
    }
}

impl core::fmt::Debug for TrackingAllocator {
    /// Prints the ledger rather than the pointer, whose value is neither stable nor informative.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_tuple("TrackingAllocator")
            .field(&self.report())
            .finish()
    }
}

impl Drop for TrackingAllocator {
    fn drop(&mut self) {
        // SAFETY: the pointer came from `Box::into_raw` in `with_limit` and this is the only
        // place it is reclaimed, so it is reclaimed exactly once. `MemZone::drop` releases any
        // block still tracked, so a session that ended early does not leak either.
        drop(unsafe { Box::from_raw(self.0) });
    }
}

impl MemZone {
    /// Releases every block still on the list and empties it.
    ///
    /// Drained iteratively rather than by dropping the chain: dropping a long
    /// `Option<Box<MemItem>>` recurses once per node, so a deep list could overflow the stack,
    /// while taking it apart in a loop cannot. Real streams hold about five blocks, but a fuzz
    /// target must not depend on that.
    fn release(&mut self) {
        let mut cursor = self.first.take();
        while let Some(mut item) = cursor {
            cursor = item.next.take();
            if let Some(layout) = tracked_layout(item.size) {
                // SAFETY: as in `mem_free` -- `item.ptr` came from `alloc` in `mem_alloc` with
                // exactly the layout rebuilt here, the node has been detached from the list, and
                // nothing else holds a reference to the block.
                unsafe { dealloc(item.ptr, layout) };
            }
        }
        self.total = 0;
    }
}

impl Drop for MemZone {
    /// Reclaims anything [`MemZone::release`] did not, so a session that panicked part-way
    /// through still does not leak. `release` leaves the list empty, so the common path is a
    /// no-op.
    fn drop(&mut self) {
        self.release();
    }
}

// =================================================================================================
//  Guard-fenced caller buffers
// =================================================================================================

/// Bytes of sentinel on each side of a [`GuardedBuf`]'s usable region.
///
/// Sixteen is comfortably wider than any single write either implementation makes through a
/// caller pointer, so an overrun of one byte and an overrun of a whole field are both caught.
pub const GUARD_LEN: usize = 16;

/// The byte a guard region is filled with.
///
/// `0x5a` is the complement of the `0xa5` a tracked allocation is filled with, which keeps a
/// guard byte distinguishable from allocator fill in a crash report.
pub const GUARD_BYTE: u8 = 0x5a;

/// A caller-owned byte buffer with a sentinel region on either side of the usable capacity.
///
/// This is the `out = malloc(len)` of `test/infcover.c` L301 with the checking a C harness
/// cannot express. The library is told about the middle only -- [`GuardedBuf::data`] and
/// [`GuardedBuf::capacity`] are what go into `next_out`, or into a `gz_header`'s `extra`,
/// `name` and `comment` together with their maxima -- so every byte of either guard that comes
/// back changed is a write past a boundary the caller declared. That is what "must never
/// over-read or over-write" reduces to for a harness with no view inside the library, and
/// [`GuardedBuf::assert_intact`] is where it is checked.
///
/// **The region is reached only through the pointer the allocator returned, never through a
/// borrow of a container.** That is the reason this is a raw allocation and lives in this file
/// rather than being a `Vec<u8>` in a caller: the pointer's provenance is the allocation
/// itself, so the library writing through a pointer it retained -- `inflateGetHeader` keeps the
/// three field pointers until the header completes, the stream is reset or the stream is ended
/// -- and this harness reading the guards afterwards cannot invalidate one another however the
/// owning value is borrowed in between. A `Vec` would give the same bytes and not that
/// property.
///
/// The value must therefore outlive every stream it was handed to, which for a caller means
/// declaring it before the stream: Rust drops locals in reverse declaration order, so a buffer
/// declared afterwards would be released while the library still held a pointer into it.
pub struct GuardedBuf {
    /// The whole allocation, guard regions included, owned as the boxed slice it came from.
    ///
    /// Held as a raw pointer rather than as a `Box<[u8]>` so that no borrow of a container is
    /// ever formed: every access below derives from this pointer, which carries the
    /// allocation's own provenance.
    storage: *mut [u8],

    /// Usable bytes between the two guards.
    capacity: usize,
}

impl GuardedBuf {
    /// Allocates a buffer of `capacity` usable bytes, sentinel-filled end to end.
    ///
    /// The usable region starts filled with [`GUARD_BYTE`] as well, which is harmless -- the
    /// library may write anything there -- and makes a stray write just inside a boundary as
    /// visible as one just outside it. A zero capacity is a real case and stays reachable:
    /// `zlib.h` L118-L133 lets a caller advertise a field buffer it has no room in.
    ///
    /// The allocation is made as a boxed slice and then taken apart with [`Box::into_raw`],
    /// which is what supplies a raw pointer with the allocation's provenance without this
    /// function having to compute a [`Layout`] -- and so without a fallible layout construction
    /// to handle. Exhaustion aborts through Rust's own allocation-failure handler, which is the
    /// right outcome for a harness that could not build its own detector.
    ///
    /// # Panics
    ///
    /// Panics if `capacity` cannot carry its guard regions. That is a condition of the harness
    /// rather than of the library: every capacity this is called with is bounded by a caller's
    /// own constant, so a failure here is a defect in the caller.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        assert!(
            capacity <= usize::MAX - 2 * GUARD_LEN,
            "guarded buffer capacity {capacity} cannot carry its guard regions"
        );
        let storage = Box::into_raw(vec![GUARD_BYTE; capacity + 2 * GUARD_LEN].into_boxed_slice());
        Self { storage, capacity }
    }

    /// Base of the whole allocation, guard regions included.
    fn base(&self) -> *mut u8 {
        self.storage.cast::<u8>()
    }

    /// The usable region's base address -- the pointer the library is given.
    ///
    /// Typed as the ABI's own `Bytef` alias rather than `*mut u8`, because that is what it
    /// becomes: a `z_stream`'s `next_out` and a `gz_header`'s `extra`, `name` and `comment` are
    /// all `Bytef *`.
    #[must_use]
    pub fn data(&self) -> *mut Bytef {
        // SAFETY: the allocation is `capacity + 2 * GUARD_LEN` bytes, so `GUARD_LEN` is an
        // offset within it; for a zero capacity the result is still inside the trailing guard.
        unsafe { self.base().add(GUARD_LEN) }
    }

    /// Usable bytes, excluding the guards -- the maximum the library is told about.
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Usable bytes as the `uInt` a `gz_header` maximum or an `avail_out` takes.
    ///
    /// Saturating rather than truncating, so a capacity that could not be advertised honestly
    /// under-reports instead of wrapping to a small number the library would then exceed.
    #[must_use]
    pub fn capacity_uint(&self) -> uInt {
        narrow_uint(self.capacity)
    }

    /// Asserts that neither guard region has been touched.
    ///
    /// `what` names the buffer, so a failure says which boundary was crossed.
    ///
    /// # Panics
    ///
    /// Panics on the first altered guard byte, reporting its offset and which guard it was in.
    /// That is the intended reporting mechanism: this runs on the harness's own stack and never
    /// inside a callback the library invokes.
    pub fn assert_intact(&self, what: &str) {
        for offset in 0..GUARD_LEN {
            // SAFETY: `offset` is below `GUARD_LEN` and `GUARD_LEN + capacity + offset` is below
            // `capacity + 2 * GUARD_LEN`, so both addresses lie inside the allocation. Both
            // bytes were initialised by `new` and can only have been overwritten since, never
            // deinitialised.
            let (leading, trailing) = unsafe {
                (
                    self.base().add(offset).read(),
                    self.base().add(GUARD_LEN + self.capacity + offset).read(),
                )
            };
            assert_eq!(
                leading, GUARD_BYTE,
                "{what}: byte {offset} of the leading guard was overwritten -- \
                 a write before the start of a caller buffer"
            );
            assert_eq!(
                trailing, GUARD_BYTE,
                "{what}: byte {offset} of the trailing guard was overwritten -- \
                 a write past the end of a caller buffer of {} bytes",
                self.capacity
            );
        }
    }
}

impl core::fmt::Debug for GuardedBuf {
    /// Prints the capacity rather than the address, which is neither stable nor informative.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("GuardedBuf")
            .field("capacity", &self.capacity)
            .field("guard_len", &GUARD_LEN)
            .finish_non_exhaustive()
    }
}

impl Drop for GuardedBuf {
    fn drop(&mut self) {
        // SAFETY: `storage` came from `Box::into_raw` over a `Box<[u8]>` in `new`, which is the
        // only place a `GuardedBuf` is built, and `Drop` runs once -- so the box is reconstituted
        // from its own pointer exactly once, with the length it was created with carried in the
        // fat pointer rather than recomputed.
        drop(unsafe { Box::from_raw(self.storage) });
    }
}

#[cfg(test)]
#[allow(
    clippy::cast_possible_truncation,
    clippy::indexing_slicing,
    clippy::unwrap_used
)]
mod tests {
    //! Enough to prove the gates and the instrumentation are wired, and no more: the
    //! comparisons against the reference belong to `tests/`, and the fuzz and bench policy to
    //! their own files.

    use super::{
        adler32, compress2, crc32, crc_table, deflate, deflate_bound, deflate_end, deflate_init2,
        deflate_pending, inflate_end, inflate_init2, uncompress, zeroed_stream, zlib_version,
        TrackingAllocator,
    };
    use libz_rs_sys::{Z_DEFAULT_STRATEGY, Z_DEFLATED, Z_FINISH, Z_OK, Z_STREAM_END};

    /// The payload `test/example.c` L35 uses, NUL included.
    const HELLO: &[u8] = b"hello, hello!\0";

    #[test]
    fn the_version_and_the_table_are_the_library_s_own() {
        assert_eq!(zlib_version().to_str().unwrap(), "1.3.2.1-motley");
        assert_eq!(crc_table().len(), 256);
        assert_eq!(crc_table()[1], 0x7707_3096);
        assert_eq!(adler32(1, b"hello"), 0x062c_0215);
        assert_eq!(crc32(0, b"hello"), 0x3610_a686);
    }

    #[test]
    fn a_stream_round_trips_through_the_gates() {
        let mut strm = zeroed_stream();
        assert_eq!(
            deflate_init2(&mut strm, 6, Z_DEFLATED, 15, 8, Z_DEFAULT_STRATEGY),
            Z_OK
        );
        assert!(deflate_bound(Some(&mut strm), HELLO.len() as _) >= HELLO.len() as _);

        let mut out = vec![0u8; 128];
        strm.next_in = HELLO.as_ptr();
        strm.avail_in = HELLO.len() as _;
        strm.next_out = out.as_mut_ptr();
        strm.avail_out = out.len() as _;
        assert_eq!(deflate(&mut strm, Z_FINISH), Z_STREAM_END);

        let mut pending = 0;
        let mut bits = 0;
        assert_eq!(deflate_pending(&mut strm, &mut pending, &mut bits), Z_OK);
        assert_eq!(pending, 0);

        let produced = out.len() - strm.avail_out as usize;
        assert_eq!(deflate_end(&mut strm), Z_OK);

        let mut back = vec![0u8; HELLO.len()];
        let (status, len) = uncompress(&mut back, &out[..produced]);
        assert_eq!(status, Z_OK);
        assert_eq!(&back[..len as usize], HELLO);
    }

    #[test]
    fn the_tracking_allocator_balances_and_reports_a_high_water_mark() {
        let allocator = TrackingAllocator::new();
        let mut strm = zeroed_stream();
        allocator.install(&mut strm);
        assert_eq!(inflate_init2(&mut strm, 15), Z_OK);
        assert!(allocator.live_bytes() > 0);
        assert_eq!(inflate_end(&mut strm), Z_OK);

        let report = allocator.finish();
        assert!(report.is_clean(), "{report:?}");
        assert!(report.high_water > 0);
        assert_eq!(report.allocations, report.frees);
    }

    #[test]
    fn an_allocator_ceiling_forces_a_refusal() {
        let allocator = TrackingAllocator::with_limit(64);
        let mut strm = zeroed_stream();
        allocator.install(&mut strm);
        assert_ne!(
            deflate_init2(&mut strm, 6, Z_DEFLATED, 15, 8, Z_DEFAULT_STRATEGY),
            Z_OK
        );
        let report = allocator.finish();
        assert!(report.refusals > 0, "{report:?}");
    }

    #[test]
    fn the_one_shot_wrapper_is_reachable_through_a_slice() {
        let mut out = vec![0u8; 64];
        let (status, len) = compress2(&mut out, HELLO, 9);
        assert_eq!(status, Z_OK);
        assert!(len > 0);
    }
}
