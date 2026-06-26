//! `libz-rs-sys` — the C-ABI drop-in shim for the safe `zlib-rs` core.
//!
//! This crate re-exposes `zlib-rs` through the exact C ABI declared in
//! `zlib.h`: `#[repr(C)]` structs that match the C layout byte-for-byte and an
//! `extern "C"` / `#[unsafe(no_mangle)]` export surface covering the public
//! zlib symbol set. Existing C consumers relink against the produced
//! `libz.so` / `libz.a` with zero source changes (AAP G4).
//!
//! # The single `unsafe` boundary
//!
//! This crate (with [`translate`]) is the ONLY place in the workspace where
//! `unsafe` is permitted (AAP §0.6.2): the `zlib-rs` core is compiled under
//! `#![forbid(unsafe_code)]`. Every export here is *thin* — it validates and
//! converts raw C pointers via [`translate`], calls the safe core, and converts
//! the result back. Every `unsafe` block carries a `// SAFETY:` justification,
//! and `#![deny(unsafe_op_in_unsafe_fn)]` (also the Rust-2024 default) forces
//! each raw operation into an explicit `unsafe {}` block.
//!
//! # `extern "C"` (not `extern "C-unwind"`)
//!
//! Every export uses plain `extern "C"`. Unwinding a Rust panic across the FFI
//! boundary into C is undefined behavior; with plain `extern "C"` a panic
//! *aborts* at the boundary instead of unwinding into C. This matches the
//! upstream `zlib-rs` project and is a deliberate safety choice.
//!
//! # The exported surface (73 symbols, AAP §0.4.1)
//!
//! 14 deflate + 16 inflate + 5 one-shot + 9 checksum + 3 version/diagnostic +
//! 26 gz file API = **73**. The 26 `gz*` functions are gated behind the
//! `gz-io` feature (on by default). `gzprintf` is the single genuinely
//! C-variadic symbol; it requires the unstable `c_variadic` language feature
//! and is therefore gated behind the (nightly-only) `c-variadic` feature and
//! omitted from the default stable build (see the `gzprintf` note below). The
//! ABI **types** (`z_stream`, `gz_header`, `gzFile`, …) live in [`zstream`].

// The C-style type and symbol names (`z_stream`, `uLong`, `gz_header_s`, ...)
// intentionally violate Rust's `CamelCase`/`snake_case` conventions to match the
// zlib ABI verbatim, as do the exported function names (`deflateInit_`, ...).
#![allow(non_camel_case_types)]
#![allow(non_snake_case)]
// Mirror the core's `no_std` capability (the C `Z_SOLO` configuration). The
// `gz*` file API additionally requires `std` and is feature-gated regardless.
#![cfg_attr(not(feature = "std"), no_std)]
// `gzprintf` is genuinely C-variadic; *defining* a variadic `extern "C"` fn
// needs the unstable `c_variadic` language feature (nightly only). Gate the
// unstable attribute so the default stable build is unaffected.
#![cfg_attr(feature = "c-variadic", feature(c_variadic))]
// Force every `unsafe` operation inside an `unsafe fn` into an explicit
// `unsafe {}` block so each carries a `// SAFETY:` justification (auditability).
// This is the Rust-2024 default; stated explicitly for intent.
#![deny(unsafe_op_in_unsafe_fn)]
// NOTE: this crate intentionally does NOT carry `#![forbid(unsafe_code)]` — it
// is the sanctioned unsafe C-ABI boundary. The core (`zlib-rs`) is the forbidden
// zone.
// Every export is `pub unsafe extern "C"` and shares ONE uniform safety
// contract: the caller must honor the zlib C API (valid/aligned pointers, the
// `z_stream`/`gz_header` lifetime rules, `next_in`/`avail_in` discipline, etc.),
// documented once in the crate prose above. Per-function `# Safety` sections
// would be 73 identical restatements, so the lint is allowed crate-wide — the
// standard convention for a `*-sys` ABI shim. The internal `// SAFETY:` blocks
// (enforced by `unsafe_op_in_unsafe_fn`) remain the per-operation justification.
#![allow(clippy::missing_safety_doc)]
// The crate/module prose intentionally cross-references private shim internals
// (the `zstream` ABI-type module, `translate::VERSION_CSTR`, the
// `gzheader_from_c` converter, …) from the docs of public `extern "C"` exports,
// so a `--document-private-items` developer build renders those links. In a
// public-only `cargo doc` build those targets are not published, which rustdoc
// would otherwise flag. The links are intentional, so allow them crate-wide
// rather than dropping the cross-references; this keeps a `-D warnings` doc
// build clean.
#![allow(rustdoc::private_intra_doc_links)]

// ===========================================================================
// Modules
// ===========================================================================

// The raw-pointer ↔ slice and integer ↔ `ReturnCode` translation layer — the
// other half of the sanctioned `unsafe` boundary. Its `pub(crate)` helpers are
// the building blocks every `extern "C"` export below consumes.
//
// `#[allow(dead_code)]`: `translate` is a complete utility layer whose surface
// is intentionally broader than any single feature build consumes — e.g.
// `gz_write_fmt` is used only under `c-variadic`, and `drop_gzfile` /
// `check_stream` / `level_from_c` are provided helpers a given consumer may not
// need. The lint would otherwise fire on those across feature combinations.
#[allow(dead_code)]
mod translate;

// Re-export the `#[repr(C)]` ABI types so they are reachable as
// `libz_rs_sys::z_stream`, `libz_rs_sys::gz_header`, `libz_rs_sys::gzFile`, … —
// cbindgen emits them and C-API users (and the doc examples) rely on
// `libz_rs_sys::z_stream::default()`.
mod zstream;
pub use zstream::*;

// ===========================================================================
// Imports
// ===========================================================================

use core::ffi::{c_char, c_int, c_long, c_uint, c_void};
// Prefer `libc` for the platform-width scalar types so the signatures match
// `zlib.h`'s `uLong` / `z_size_t` / `z_off_t` byte-for-byte (these differ from
// fixed-width types on LLP64 / large-file builds), exactly as `zstream` does.
use libc::{c_ulong, off_t, size_t};

// `Box` is in the prelude under `std`; under `no_std` it comes from `alloc`.
#[cfg(not(feature = "std"))]
extern crate alloc;
#[cfg(not(feature = "std"))]
use alloc::boxed::Box;
// `Vec` is in the prelude under `std`; under `no_std` it comes from `alloc`. It
// is named only by the gzip header-capture helper (`gzheader_capture_from_c`),
// so the import is additionally gated on `gzip` to avoid an unused-import lint
// in a `no_std` build without the gzip feature.
#[cfg(all(not(feature = "std"), feature = "gzip"))]
use alloc::vec::Vec;
// `Rc` carries the C-allocator adapter into the inflate state (which must stay
// `Clone` for `inflateCopy`); see `inflateInit2_`. It lives in `std::rc` under
// `std` and in `alloc::rc` under `no_std`.
#[cfg(not(feature = "std"))]
use alloc::rc::Rc;
#[cfg(feature = "std")]
use std::rc::Rc;

// Safe-core types consumed by the exports.
use zlib_rs::{DataType, Flush, GzHeader, ReturnCode, Strategy, ZStream, ZlibError};

// Bring the `pub(crate)` translation helpers into scope for terse call sites.
use translate::{
    attach_state, check_version, input_slice, output_slice, stream_state, to_c_int, version_ptr,
    z_error_cstr,
};

// ===========================================================================
// Public integer constants (mirror `zlib.h` L172-L216)
// ===========================================================================
//
// Each constant is anchored to the single source of truth in
// `zlib_rs::constants` (the enums via `.as_i32()`, the bare values directly) so
// the C ABI can never drift from the core. cbindgen emits these into the
// regenerated header, and `libz_rs_sys::Z_OK` &c. exist for Rust callers. The
// `const _` assertions below pin the literal C values at compile time.

// --- Flush modes (`Flush` enum) ---
pub const Z_NO_FLUSH: c_int = Flush::NoFlush.as_i32();
pub const Z_PARTIAL_FLUSH: c_int = Flush::PartialFlush.as_i32();
pub const Z_SYNC_FLUSH: c_int = Flush::SyncFlush.as_i32();
pub const Z_FULL_FLUSH: c_int = Flush::FullFlush.as_i32();
pub const Z_FINISH: c_int = Flush::Finish.as_i32();
pub const Z_BLOCK: c_int = Flush::Block.as_i32();
pub const Z_TREES: c_int = Flush::Trees.as_i32();

// --- Return / error codes (`ReturnCode` / `ZlibError`) ---
pub const Z_OK: c_int = ReturnCode::Ok.as_i32();
pub const Z_STREAM_END: c_int = ReturnCode::StreamEnd.as_i32();
pub const Z_NEED_DICT: c_int = ReturnCode::NeedDict.as_i32();
pub const Z_ERRNO: c_int = ZlibError::ErrNo.as_i32();
pub const Z_STREAM_ERROR: c_int = ZlibError::StreamError.as_i32();
pub const Z_DATA_ERROR: c_int = ZlibError::DataError.as_i32();
pub const Z_MEM_ERROR: c_int = ZlibError::MemError.as_i32();
pub const Z_BUF_ERROR: c_int = ZlibError::BufError.as_i32();
pub const Z_VERSION_ERROR: c_int = ZlibError::VersionError.as_i32();

// --- Compression levels ---
pub const Z_NO_COMPRESSION: c_int = zlib_rs::constants::Z_NO_COMPRESSION;
pub const Z_BEST_SPEED: c_int = zlib_rs::constants::Z_BEST_SPEED;
pub const Z_BEST_COMPRESSION: c_int = zlib_rs::constants::Z_BEST_COMPRESSION;
pub const Z_DEFAULT_COMPRESSION: c_int = zlib_rs::constants::Z_DEFAULT_COMPRESSION;

// --- Compression strategies (`Strategy` enum) ---
pub const Z_FILTERED: c_int = Strategy::Filtered.as_i32();
pub const Z_HUFFMAN_ONLY: c_int = Strategy::HuffmanOnly.as_i32();
pub const Z_RLE: c_int = Strategy::Rle.as_i32();
pub const Z_FIXED: c_int = Strategy::Fixed.as_i32();
pub const Z_DEFAULT_STRATEGY: c_int = Strategy::Default.as_i32();

// --- Data types (`DataType` enum) ---
pub const Z_BINARY: c_int = DataType::Binary.as_i32();
pub const Z_TEXT: c_int = DataType::Text.as_i32();
/// `Z_ASCII` is the historical alias of [`Z_TEXT`].
pub const Z_ASCII: c_int = Z_TEXT;
pub const Z_UNKNOWN: c_int = DataType::Unknown.as_i32();

// --- Method and the NULL sentinel ---
pub const Z_DEFLATED: c_int = zlib_rs::constants::Z_DEFLATED;
pub const Z_NULL: c_int = zlib_rs::constants::Z_NULL;

// Compile-time proof that the anchored values equal the canonical `zlib.h`
// literals (a drift in the core would fail the build here rather than silently
// shipping a wrong ABI constant).
const _: () = {
    assert!(Z_NO_FLUSH == 0 && Z_PARTIAL_FLUSH == 1 && Z_SYNC_FLUSH == 2);
    assert!(Z_FULL_FLUSH == 3 && Z_FINISH == 4 && Z_BLOCK == 5 && Z_TREES == 6);
    assert!(Z_OK == 0 && Z_STREAM_END == 1 && Z_NEED_DICT == 2);
    assert!(Z_ERRNO == -1 && Z_STREAM_ERROR == -2 && Z_DATA_ERROR == -3);
    assert!(Z_MEM_ERROR == -4 && Z_BUF_ERROR == -5 && Z_VERSION_ERROR == -6);
    assert!(Z_NO_COMPRESSION == 0 && Z_BEST_SPEED == 1 && Z_BEST_COMPRESSION == 9);
    assert!(Z_DEFAULT_COMPRESSION == -1);
    assert!(Z_DEFAULT_STRATEGY == 0 && Z_FILTERED == 1 && Z_HUFFMAN_ONLY == 2);
    assert!(Z_RLE == 3 && Z_FIXED == 4);
    assert!(Z_BINARY == 0 && Z_TEXT == 1 && Z_ASCII == 1 && Z_UNKNOWN == 2);
    assert!(Z_DEFLATED == 8 && Z_NULL == 0);
};

// ===========================================================================
// Version constants
// ===========================================================================

/// The runtime ABI version string, exactly `"1.3.2.1-motley"` (the canonical
/// `#define ZLIB_VERSION`). Distinct from the crate's Cargo semver. Anchored to
/// [`translate::VERSION_CSTR`] (itself proven equal to
/// `zlib_rs::constants::ZLIB_VERSION` at compile time).
pub const ZLIB_VERSION: &core::ffi::CStr =
    match core::ffi::CStr::from_bytes_with_nul(translate::VERSION_CSTR) {
        Ok(s) => s,
        Err(_) => panic!("VERSION_CSTR must be NUL-terminated"),
    };

/// The numeric ABI version, `0x1321` (the canonical `#define ZLIB_VERNUM`).
pub const ZLIB_VERNUM: c_int = 0x1321;

const _: () = assert!(ZLIB_VERNUM == zlib_rs::constants::ZLIB_VERNUM as c_int);

// ===========================================================================
// `zlibCompileFlags` backing helper (preserved from the foundation milestone)
// ===========================================================================

/// The compile-time configuration bitmask, reported with **this crate's**
/// `z_off_t` (== [`libc::off_t`]) definition.
///
/// The pure-Rust core's `zlib_rs::util::version::zlib_compile_flags` reports the
/// C `long` width for the `z_off_t` size field (bits 6-7). The C ABI maps
/// `z_off_t` to [`libc::off_t`], which is **not** always `long`-width (it
/// differs on 64-bit Windows / LLP64 and 32-bit large-file builds). This helper
/// therefore computes the flags via the parameterized
/// `zlib_rs::util::version::zlib_compile_flags_for`, passing `size_of::<off_t>()`,
/// so the value the C ABI advertises matches the `z_off_t` it actually exposes.
/// It backs the exported [`zlibCompileFlags`] symbol.
#[must_use]
pub fn zlib_compile_flags() -> c_ulong {
    zlib_rs::util::version::zlib_compile_flags_for(core::mem::size_of::<off_t>()) as c_ulong
}

// ===========================================================================
// `inflateGetHeader` C-struct write-back registry (std only)
// ===========================================================================
//
// The C `inflateGetHeader(strm, head)` contract is that `inflate` fills the
// caller's `*mut gz_header` as it parses the gzip header. The safe core instead
// captures the header into an owned `GzHeader` inside the `InflateState` (no
// slot exists for a raw C pointer). To honor the C contract faithfully we keep
// a process-global, `std`-gated registry mapping a `z_stream` pointer to the
// caller's `gz_header` pointer; the `inflate` export copies the core's captured
// header back into that C struct after each call, and `inflateEnd` unregisters.
//
// Pointers are stored as `usize` so the map is `Send`/`Sync`; the raw pointers
// are only dereferenced on the thread driving the stream (C serializes
// per-stream access). The entry is removed in `inflateEnd` before the address
// could ever be recycled, so there is no ABA hazard. Under `no_std` (where no
// registry is available) `inflateGetHeader` still enables core capture but
// performs no C-struct write-back (documented limitation).
//
// Gated on BOTH `std` (the registry needs `std::sync`) AND `gzip` (there is no
// gzip header to write back without the core's gzip-capture path). A std-only
// build — `--no-default-features --features std` — therefore omits this module
// entirely instead of referencing the gzip-gated `InflateState::header()`.
#[cfg(all(feature = "std", feature = "gzip"))]
mod header_reg {
    use super::{gz_header, z_streamp};
    use std::collections::HashMap;
    use std::sync::{LazyLock, Mutex};

    static REG: LazyLock<Mutex<HashMap<usize, usize>>> =
        LazyLock::new(|| Mutex::new(HashMap::new()));

    /// Record that `strm` wants its gzip header written back into `head`.
    pub(super) fn register(strm: z_streamp, head: *mut gz_header) {
        if let Ok(mut map) = REG.lock() {
            map.insert(strm as usize, head as usize);
        }
    }

    /// Look up the caller's `gz_header` pointer for `strm`, if registered.
    pub(super) fn lookup(strm: z_streamp) -> Option<*mut gz_header> {
        let map = REG.lock().ok()?;
        map.get(&(strm as usize)).map(|&p| p as *mut gz_header)
    }

    /// Drop any registration for `strm` (called from `inflateEnd`).
    pub(super) fn unregister(strm: z_streamp) {
        if let Ok(mut map) = REG.lock() {
            map.remove(&(strm as usize));
        }
    }
}

/// An owned snapshot of a captured [`GzHeader`], used to write the parsed gzip
/// header back into a caller-supplied C `gz_header` without holding a borrow of
/// the inflate state across the raw-pointer writes.
///
/// Requires both `std` (owned `Vec` copies) and `gzip` (the captured
/// [`GzHeader`] only exists under the core's gzip feature).
#[cfg(all(feature = "std", feature = "gzip"))]
struct HeaderSnapshot {
    text: c_int,
    time: c_ulong,
    xflags: c_int,
    os: c_int,
    hcrc: c_int,
    done: c_int,
    extra: Option<Vec<u8>>,
    name: Option<Vec<u8>>,
    comment: Option<Vec<u8>>,
}

#[cfg(all(feature = "std", feature = "gzip"))]
impl HeaderSnapshot {
    /// Capture the relevant fields of `h` into owned copies.
    fn capture(h: &GzHeader) -> Self {
        Self {
            text: c_int::from(h.text),
            time: h.time as c_ulong,
            xflags: h.xflags as c_int,
            os: h.os as c_int,
            hcrc: c_int::from(h.hcrc),
            done: c_int::from(h.done),
            extra: h.extra.clone(),
            name: h.name.clone(),
            comment: h.comment.clone(),
        }
    }

    /// Write the snapshot into the caller's C `gz_header`, mirroring the field
    /// fill done by C `inflate` (extra/name/comment copied up to the caller's
    /// `*_max`, names/comments NUL-terminated; `extra_len` reports the true
    /// length even when the buffer is too small).
    ///
    /// # Safety
    ///
    /// `head` must be the caller's valid, writable `gz_header` pointer (the one
    /// registered via `inflateGetHeader`), with `extra`/`name`/`comment` either
    /// null or valid for their stated `*_max` lengths.
    unsafe fn write_to(&self, head: *mut gz_header) {
        // SAFETY: `head` is the caller-registered, valid `gz_header` (contract
        // above); each field write is in-bounds for that struct.
        unsafe {
            (*head).text = self.text;
            (*head).time = self.time;
            (*head).xflags = self.xflags;
            (*head).os = self.os;
            (*head).hcrc = self.hcrc;

            if let Some(extra) = self.extra.as_ref() {
                (*head).extra_len = extra.len() as c_uint;
                let dst = (*head).extra;
                let max = (*head).extra_max as usize;
                if !dst.is_null() && max > 0 {
                    let n = core::cmp::min(extra.len(), max);
                    // SAFETY: `dst` is valid for `extra_max ≥ n` bytes (caller
                    // contract); `extra` is a distinct owned buffer of ≥ n bytes.
                    core::ptr::copy_nonoverlapping(extra.as_ptr(), dst, n);
                }
            }

            if let Some(name) = self.name.as_ref() {
                let dst = (*head).name;
                let max = (*head).name_max as usize;
                if !dst.is_null() && max > 0 {
                    let n = core::cmp::min(name.len(), max - 1);
                    // SAFETY: `dst` is valid for `name_max ≥ n + 1` bytes; the
                    // trailing NUL is written at index `n < name_max`.
                    core::ptr::copy_nonoverlapping(name.as_ptr(), dst, n);
                    *dst.add(n) = 0;
                }
            }

            if let Some(comment) = self.comment.as_ref() {
                let dst = (*head).comment;
                let max = (*head).comm_max as usize;
                if !dst.is_null() && max > 0 {
                    let n = core::cmp::min(comment.len(), max - 1);
                    // SAFETY: `dst` is valid for `comm_max ≥ n + 1` bytes; the
                    // trailing NUL is written at index `n < comm_max`.
                    core::ptr::copy_nonoverlapping(comment.as_ptr(), dst, n);
                    *dst.add(n) = 0;
                }
            }

            (*head).done = self.done;
        }
    }
}

// ===========================================================================
// Shared private helpers (raw ↔ safe conversions used by several exports)
// ===========================================================================

/// Mirror C `deflateReset`/`inflateResetKeep` publishing of the reset scalars
/// from the (Rust) [`ZStream`] onto the caller's C `z_stream`.
///
/// # Safety
///
/// `strm` must be a valid, non-null `z_streamp` whose state was just reset.
unsafe fn sync_reset_scalars(strm: z_streamp, z: &ZStream) {
    // SAFETY: `strm` is valid and non-null (the caller recovered its state);
    // each field write is in-bounds for a `z_stream`.
    unsafe {
        (*strm).total_in = z.total_in as c_ulong;
        (*strm).total_out = z.total_out as c_ulong;
        (*strm).adler = z.adler as c_ulong;
        (*strm).data_type = z.data_type as c_int;
        (*strm).msg = core::ptr::null();
    }
}

/// Per-call write-back for `deflateParams` (the private `translate::write_back`
/// is not reachable here, so the identical protocol is reproduced verbatim):
/// advance `next_in`/`next_out`, decrement `avail_*`, bump `total_*`, copy the
/// post-call `adler`/`data_type`, and set `msg` from the result code.
///
/// # Safety
///
/// `strm` must be valid and non-null; `consumed ≤ avail_in` and
/// `produced ≤ avail_out` (the engine guarantees this).
unsafe fn params_write_back(
    strm: z_streamp,
    consumed: usize,
    produced: usize,
    adler: c_ulong,
    data_type: c_int,
    result: Result<ReturnCode, ZlibError>,
) {
    // SAFETY: `strm` is valid/non-null; the `.add` offsets are in-bounds because
    // `consumed ≤ avail_in` / `produced ≤ avail_out`; advancement is skipped on a
    // zero count so a null `next_in`/`next_out` is never offset.
    unsafe {
        let s = &mut *strm;
        if consumed != 0 {
            s.next_in = s.next_in.add(consumed);
        }
        s.avail_in -= consumed as c_uint;
        s.total_in = s.total_in.wrapping_add(consumed as c_ulong);
        if produced != 0 {
            s.next_out = s.next_out.add(produced);
        }
        s.avail_out -= produced as c_uint;
        s.total_out = s.total_out.wrapping_add(produced as c_ulong);
        s.adler = adler;
        s.data_type = data_type;
        s.msg = match result {
            Ok(_) => core::ptr::null(),
            Err(err) => z_error_cstr(err.as_i32()),
        };
    }
}

/// Build a core [`GzHeader`] from a caller's C `gz_header` (for
/// `deflateSetHeader`). `extra` is read for `extra_len` bytes; `name`/`comment`
/// are read as NUL-terminated C strings.
///
/// # Safety
///
/// `head` must be a valid, readable `gz_header`; when non-null its
/// `extra` is valid for `extra_len` bytes and `name`/`comment` are
/// NUL-terminated.
unsafe fn gzheader_from_c(head: *const gz_header) -> GzHeader {
    // SAFETY: `head` is a valid, readable `gz_header` (caller contract).
    unsafe {
        let mut gh = GzHeader::new()
            .with_text((*head).text != 0)
            .with_time((*head).time as u32)
            .with_xflags((*head).xflags)
            .with_os((*head).os)
            .with_hcrc((*head).hcrc != 0);

        let extra = (*head).extra;
        if !extra.is_null() {
            let len = (*head).extra_len as usize;
            // SAFETY: `extra` is valid for `extra_len` bytes when non-null.
            let bytes = core::slice::from_raw_parts(extra, len).to_vec();
            gh = gh.with_extra(bytes);
        }

        let name = (*head).name;
        if !name.is_null() {
            // SAFETY: `name` is a NUL-terminated C string when non-null.
            let bytes = core::ffi::CStr::from_ptr(name as *const c_char)
                .to_bytes()
                .to_vec();
            gh = gh.with_name(bytes);
        }

        let comment = (*head).comment;
        if !comment.is_null() {
            // SAFETY: `comment` is a NUL-terminated C string when non-null.
            let bytes = core::ffi::CStr::from_ptr(comment as *const c_char)
                .to_bytes()
                .to_vec();
            gh = gh.with_comment(bytes);
        }

        gh
    }
}

/// Build a core [`GzHeader`] configured to *capture* the gzip-header fields a
/// caller requested via `inflateGetHeader`.
///
/// In the C ABI a caller opts into capturing a variable-length field by setting
/// the corresponding `gz_header` buffer pointer (`name` / `comment` / `extra`)
/// to a non-null address with a positive capacity (`name_max` / `comm_max` /
/// `extra_max`); a null pointer means "do not capture this field" and `inflate`
/// then consumes and discards those header bytes. The safe core models that
/// opt-in as `Option<Vec<u8>>`: a `Some(_)` buffer is captured into, a `None`
/// field is parsed-and-discarded. This helper bridges the two representations,
/// allocating a capture buffer pre-sized to the caller's `*_max` for each field
/// the caller asked for and leaving the rest `None`.
///
/// The owned `Vec`s grow as `inflate` parses (the core is unaware of `*_max`);
/// the per-field `*_max` clamp and NUL-termination are applied later, when
/// [`HeaderSnapshot::write_to`] copies the captured bytes back into the caller's
/// fixed C buffers. Pre-sizing with `with_capacity(*_max)` simply avoids
/// reallocation for the common case where the field fits.
///
/// # Safety
///
/// `head` must be a valid, readable `gz_header`.
#[cfg(feature = "gzip")]
unsafe fn gzheader_capture_from_c(head: *const gz_header) -> GzHeader {
    let mut gh = GzHeader::new();
    // SAFETY: `head` is a valid, readable `gz_header` (caller contract); each
    // field read is in-bounds for that struct. The buffer pointers are only
    // tested for null and their `*_max` capacities read here — never
    // dereferenced — so an opt-in `Some(Vec)` is created without touching the
    // caller's (possibly uninitialized) buffer memory.
    unsafe {
        if !(*head).extra.is_null() && (*head).extra_max > 0 {
            gh.extra = Some(Vec::with_capacity((*head).extra_max as usize));
        }
        if !(*head).name.is_null() && (*head).name_max > 0 {
            gh.name = Some(Vec::with_capacity((*head).name_max as usize));
        }
        if !(*head).comment.is_null() && (*head).comm_max > 0 {
            gh.comment = Some(Vec::with_capacity((*head).comm_max as usize));
        }
    }
    gh
}

/// Copy the public `z_stream` scalar fields from `source` to `dest` (for
/// `deflateCopy`/`inflateCopy`, mirroring C's `*dest = *source`).
///
/// # Safety
///
/// `dest` and `source` must both be valid, non-null `z_streamp`s.
unsafe fn copy_stream_scalars(dest: z_streamp, source: z_streamp) {
    // SAFETY: both are valid, non-null `z_stream`s (caller contract).
    unsafe {
        (*dest).next_in = (*source).next_in;
        (*dest).avail_in = (*source).avail_in;
        (*dest).total_in = (*source).total_in;
        (*dest).next_out = (*source).next_out;
        (*dest).avail_out = (*source).avail_out;
        (*dest).total_out = (*source).total_out;
        (*dest).data_type = (*source).data_type;
        (*dest).adler = (*source).adler;
        (*dest).msg = (*source).msg;
    }
}

// ===========================================================================
// AAP-Phase 1 — deflate family (14 symbols)
// ===========================================================================

/// `deflateInit2_` — full compressor initialization (`zlib.h` L1907).
///
/// Validates the version/`sizeof(z_stream)` guard and a non-null `strm`, maps
/// the C `strategy` to the core [`Strategy`], allocates the owned state, and
/// stashes it in `z_stream.state`. `windowBits` overloading (zlib/raw/gzip/auto)
/// is handled by the core — the raw value is passed straight through.
///
/// # Safety
///
/// `strm` (if non-null) and `version` must satisfy the C `deflateInit2_`
/// contract (a valid `z_stream` and a NUL-terminated version string).
#[unsafe(no_mangle)]
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
    // SAFETY: `version` is the caller-supplied NUL-terminated version string.
    if let Err(code) = unsafe { check_version(version, stream_size) } {
        return code;
    }
    if strm.is_null() {
        return Z_STREAM_ERROR;
    }
    let strategy = match translate::strategy_from_c(strategy) {
        Some(s) => s,
        None => return Z_STREAM_ERROR,
    };
    let mut state = Box::new(ZStream::new());
    // Honor caller-installed custom allocation: if the C `z_stream` carries a
    // non-null `zalloc`, build an adapter that routes the core's buffer
    // allocations through `zalloc`/`zfree`/`opaque` (and maps a failed
    // allocation to `Z_MEM_ERROR`). With no custom `zalloc`, the core uses the
    // global allocator.
    // SAFETY: `strm` is non-null (checked above); reading the C-ABI
    // `zalloc`/`zfree`/`opaque` fields is valid per the `z_stream` contract.
    let (zalloc, zfree, opaque) = unsafe { ((*strm).zalloc, (*strm).zfree, (*strm).opaque) };
    if let Some(adapter) = translate::CAllocator::from_callbacks(zalloc, zfree, opaque) {
        state.set_allocator(Box::new(adapter));
    }
    match zlib_rs::deflate::deflate_init2(&mut state, level, method, windowBits, memLevel, strategy)
    {
        Ok(_) => {
            // SAFETY: `strm` is non-null (checked); transfer ownership of the
            // freshly-initialized state into the C `state` field.
            unsafe { attach_state(strm, state) };
            // SAFETY: `strm` is non-null; clear any stale error message.
            unsafe { (*strm).msg = core::ptr::null() };
            Z_OK
        }
        Err(e) => e.as_i32(),
    }
}

/// `deflateInit_` — convenience initializer (`zlib.h` L1903): `deflateInit2_`
/// with `method = Z_DEFLATED`, `windowBits = MAX_WBITS`,
/// `memLevel = DEF_MEM_LEVEL`, `strategy = Z_DEFAULT_STRATEGY`.
///
/// # Safety
///
/// As [`deflateInit2_`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn deflateInit_(
    strm: z_streamp,
    level: c_int,
    version: *const c_char,
    stream_size: c_int,
) -> c_int {
    // SAFETY: forwarded to `deflateInit2_`, whose contract this inherits.
    unsafe {
        deflateInit2_(
            strm,
            level,
            Z_DEFLATED,
            zlib_rs::constants::MAX_WBITS,
            zlib_rs::constants::DEF_MEM_LEVEL,
            Z_DEFAULT_STRATEGY,
            version,
            stream_size,
        )
    }
}

/// `deflate` — compress/flush one step (`zlib.h` L744).
///
/// # Safety
///
/// `strm` must be null or a valid `z_streamp` whose `next_in`/`next_out` honor
/// `avail_in`/`avail_out`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn deflate(strm: z_streamp, flush: c_int) -> c_int {
    // SAFETY: `strm` is a caller-supplied z_streamp; `run_deflate` validates the
    // null/state pointers and upholds the buffer-length contract.
    unsafe { translate::run_deflate(strm, flush) }
}

/// `deflateEnd` — free the compressor (`zlib.h` L759). Returns `Z_DATA_ERROR`
/// if called mid-stream (pending output), `Z_STREAM_ERROR` for an invalid
/// state, else `Z_OK`.
///
/// # Safety
///
/// `strm` must be null or a valid `z_streamp`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn deflateEnd(strm: z_streamp) -> c_int {
    // SAFETY: caller-supplied z_streamp; `stream_state` checks null/state.
    let z = match unsafe { stream_state(strm) } {
        Some(z) => z,
        None => return Z_STREAM_ERROR,
    };
    let result = zlib_rs::deflate::deflate_end(z);
    if matches!(result, Err(ZlibError::StreamError)) {
        // Not a deflate stream: the inner state is untouched, so the outer box
        // must NOT be freed here (the caller will use `inflateEnd`). Matches C
        // returning `Z_STREAM_ERROR` for an invalid `deflateStateCheck`.
        return Z_STREAM_ERROR;
    }
    // `deflate_end` freed the inner deflate state (via `ZStream::end`); now
    // reclaim and drop the OUTER `Box<ZStream>` and null `z_stream.state`.
    // SAFETY: `strm` still owns the outer box pointer; `drop_state` reconstructs
    // and drops it exactly once.
    unsafe { translate::drop_state(strm) };
    to_c_int(result)
}

/// `deflateReset` — reset for a fresh stream, keeping allocations (`zlib.h`
/// L1010).
///
/// # Safety
///
/// `strm` must be null or a valid `z_streamp`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn deflateReset(strm: z_streamp) -> c_int {
    // SAFETY: caller-supplied z_streamp; `stream_state` checks null/state.
    let z = match unsafe { stream_state(strm) } {
        Some(z) => z,
        None => return Z_STREAM_ERROR,
    };
    let result = zlib_rs::deflate::deflate_reset(z);
    if result.is_ok() {
        // SAFETY: `strm` is valid (state recovered above). `deflate_reset` zeroed
        // the (Rust) `ZStream` counters and set `adler`; mirror them onto the C
        // struct so a caller reading `total_in`/`adler` sees the reset values.
        unsafe { sync_reset_scalars(strm, z) };
    }
    to_c_int(result)
}

/// `deflateParams` — change level/strategy mid-stream (`zlib.h` L867), flushing
/// pending output through `next_out` if required. Forms the input/output slices
/// and writes the per-call results back exactly as `deflate` does.
///
/// # Safety
///
/// `strm` must be null or a valid `z_streamp` whose buffers honor `avail_*`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn deflateParams(strm: z_streamp, level: c_int, strategy: c_int) -> c_int {
    // SAFETY: caller-supplied z_streamp; `stream_state` checks null/state.
    let z = match unsafe { stream_state(strm) } {
        Some(z) => z,
        None => return Z_STREAM_ERROR,
    };
    let strategy = match translate::strategy_from_c(strategy) {
        Some(s) => s,
        None => return Z_STREAM_ERROR,
    };
    // SAFETY: per the stream contract, `next_in`/`next_out` are valid for
    // `avail_in`/`avail_out` bytes; the borrows live only across the core call.
    let (input, output) = unsafe {
        (
            input_slice((*strm).next_in, (*strm).avail_in),
            output_slice((*strm).next_out, (*strm).avail_out),
        )
    };
    let (result, consumed, produced) =
        zlib_rs::deflate::deflate_params(z, level, strategy, input, output);
    let adler = z.adler as c_ulong;
    let data_type = z.data_type as c_int;
    // SAFETY: `strm` valid; `consumed ≤ avail_in`, `produced ≤ avail_out`.
    unsafe { params_write_back(strm, consumed, produced, adler, data_type, result) };
    to_c_int(result)
}

/// `deflateBound` — upper bound on the compressed size of `sourceLen` bytes
/// (`zlib.h` L768). Uses the stream's exact bound when a deflate state is
/// present, else the conservative one-shot bound.
///
/// # Safety
///
/// `strm` must be null or a valid `z_streamp`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn deflateBound(strm: z_streamp, sourceLen: c_ulong) -> c_ulong {
    // `c_ulong as u64` is a no-op on LP64 but a real widening on LLP64 (Win64,
    // where `uLong`/`c_ulong` is 32-bit); the lint only observes the LP64 build.
    #[allow(clippy::unnecessary_cast)]
    let source_len = sourceLen as u64;
    // SAFETY: caller-supplied z_streamp; `stream_state` checks null/state.
    let bound = match unsafe { stream_state(strm) } {
        Some(z) => match z.deflate_state() {
            Some(s) => s.deflate_bound(source_len),
            None => zlib_rs::util::compress_bound(source_len as usize) as u64,
        },
        None => zlib_rs::util::compress_bound(source_len as usize) as u64,
    };
    bound as c_ulong
}

/// `deflatePrime` — insert bits into the output stream (`zlib.h` L1031).
///
/// # Safety
///
/// `strm` must be null or a valid `z_streamp`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn deflatePrime(strm: z_streamp, bits: c_int, value: c_int) -> c_int {
    // SAFETY: caller-supplied z_streamp; `stream_state` checks null/state.
    let z = match unsafe { stream_state(strm) } {
        Some(z) => z,
        None => return Z_STREAM_ERROR,
    };
    to_c_int(zlib_rs::deflate::deflate_prime(z, bits, value))
}

/// `deflateSetDictionary` — preset the compressor dictionary (`zlib.h` L633).
///
/// # Safety
///
/// `strm` must be null or a valid `z_streamp`; `(dictionary, dictLength)` must
/// be a valid buffer (or `dictionary` null with `dictLength == 0`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn deflateSetDictionary(
    strm: z_streamp,
    dictionary: *const u8,
    dictLength: c_uint,
) -> c_int {
    // SAFETY: caller-supplied z_streamp; `stream_state` checks null/state.
    let z = match unsafe { stream_state(strm) } {
        Some(z) => z,
        None => return Z_STREAM_ERROR,
    };
    // SAFETY: `(dictionary, dictLength)` is a valid C buffer (or null → empty).
    let dict = unsafe { input_slice(dictionary, dictLength) };
    to_c_int(zlib_rs::deflate::deflate_set_dictionary(z, dict))
}

/// `deflateGetDictionary` — read back the sliding-window dictionary (`zlib.h`
/// L658). Writes the dictionary length through `dictLength` and, when
/// `dictionary` is non-null, copies that many bytes into it.
///
/// # Safety
///
/// `strm` must be null or a valid `z_streamp`. When non-null, `dictionary` must
/// be writable for the returned dictionary length and `dictLength` writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn deflateGetDictionary(
    strm: z_streamp,
    dictionary: *mut u8,
    dictLength: *mut c_uint,
) -> c_int {
    // SAFETY: caller-supplied z_streamp; `stream_state` checks null/state.
    let z = match unsafe { stream_state(strm) } {
        Some(z) => z,
        None => return Z_STREAM_ERROR,
    };
    let dlen = match zlib_rs::deflate::deflate_get_dictionary(z, None) {
        Ok(l) => l,
        Err(e) => return e.as_i32(),
    };
    if !dictionary.is_null() {
        // SAFETY: the caller guarantees `dictionary` is valid for ≥ `dlen` bytes.
        let buf = unsafe { output_slice(dictionary, dlen as c_uint) };
        let _ = zlib_rs::deflate::deflate_get_dictionary(z, Some(buf));
    }
    if !dictLength.is_null() {
        // SAFETY: the caller guarantees `dictLength` is a valid writable pointer.
        unsafe { *dictLength = dlen as c_uint };
    }
    Z_OK
}

/// `deflateSetHeader` — supply a gzip header for a `windowBits` gzip stream
/// (`zlib.h` L835). The core requires `wrap == 2` (gzip) and returns
/// `Z_STREAM_ERROR` otherwise.
///
/// # Safety
///
/// `strm` must be null or a valid `z_streamp`; `head` (if non-null) a valid
/// `gz_header` per [`gzheader_from_c`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn deflateSetHeader(strm: z_streamp, head: gz_headerp) -> c_int {
    // SAFETY: caller-supplied z_streamp; `stream_state` checks null/state.
    let z = match unsafe { stream_state(strm) } {
        Some(z) => z,
        None => return Z_STREAM_ERROR,
    };
    if head.is_null() {
        return Z_STREAM_ERROR;
    }
    // SAFETY: `head` is non-null (checked) and a valid `gz_header`.
    let gh = unsafe { gzheader_from_c(head) };
    to_c_int(zlib_rs::deflate::deflate_set_header(z, gh))
}

/// `deflateCopy` — duplicate a compressor stream (`zlib.h` L991).
///
/// # Safety
///
/// `dest` and `source` must be null or valid `z_streamp`s.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn deflateCopy(dest: z_streamp, source: z_streamp) -> c_int {
    if dest.is_null() {
        return Z_STREAM_ERROR;
    }
    // SAFETY: caller-supplied z_streamp; `stream_state` checks null/state.
    let src = match unsafe { stream_state(source) } {
        Some(z) => z,
        None => return Z_STREAM_ERROR,
    };
    let mut dz = Box::new(ZStream::new());
    match zlib_rs::deflate::deflate_copy(&mut dz, src) {
        Ok(_) => {
            // SAFETY: `dest` is non-null (checked); transfer the cloned state.
            unsafe { attach_state(dest, dz) };
            // SAFETY: `dest` and `source` are valid, non-null z_streamps.
            unsafe { copy_stream_scalars(dest, source) };
            Z_OK
        }
        Err(e) => e.as_i32(),
    }
}

/// `deflateTune` — fine-tune the internal match parameters (`zlib.h` L786).
///
/// # Safety
///
/// `strm` must be null or a valid `z_streamp`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn deflateTune(
    strm: z_streamp,
    good_length: c_int,
    max_lazy: c_int,
    nice_length: c_int,
    max_chain: c_int,
) -> c_int {
    // SAFETY: caller-supplied z_streamp; `stream_state` checks null/state.
    let z = match unsafe { stream_state(strm) } {
        Some(z) => z,
        None => return Z_STREAM_ERROR,
    };
    to_c_int(zlib_rs::deflate::deflate_tune(
        z,
        good_length,
        max_lazy,
        nice_length,
        max_chain,
    ))
}

/// `deflatePending` — report bytes/bits pending output (`zlib.h` L1019).
///
/// # Safety
///
/// `strm` must be null or a valid `z_streamp`; `pending`/`bits` null or valid
/// writable pointers.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn deflatePending(
    strm: z_streamp,
    pending: *mut c_uint,
    bits: *mut c_int,
) -> c_int {
    // SAFETY: caller-supplied z_streamp; `stream_state` checks null/state.
    let z = match unsafe { stream_state(strm) } {
        Some(z) => z,
        None => return Z_STREAM_ERROR,
    };
    match zlib_rs::deflate::deflate_pending(z) {
        Ok((p, b)) => {
            if !pending.is_null() {
                // SAFETY: the caller guarantees `pending` is a valid pointer.
                unsafe { *pending = p as c_uint };
            }
            if !bits.is_null() {
                // SAFETY: the caller guarantees `bits` is a valid pointer.
                unsafe { *bits = b as c_int };
            }
            Z_OK
        }
        Err(e) => e.as_i32(),
    }
}

// ===========================================================================
// AAP-Phase 2 — inflate family (16 symbols)
//
// Each export is a thin C-ABI wrapper: guard null/version via `translate`,
// convert C scalars, call the safe `zlib_rs` inflate core, convert the result
// back. Decompression is driven by `translate::run_inflate`, whose tuple order
// is (consumed, produced, result) — distinct from `run_deflate`'s
// (result, consumed, produced). `translate::run_inflate` performs the
// next_in/next_out/total_*/adler/msg/data_type write-back internally.
// ===========================================================================

/// Copy the core's captured gzip header back into the caller's registered C
/// `gz_header` (set via `inflateGetHeader`). No-op when nothing is registered.
///
/// std-only: the registry that maps `z_stream*` → `gz_header*` requires
/// `std::sync`. Under `no_std` the core still captures the header internally,
/// but it cannot be written back through the C struct (documented limitation).
///
/// Requires both `std` (the registry) and `gzip` (the core's captured header,
/// `InflateState::header()`); a std-only build omits this function entirely.
#[cfg(all(feature = "std", feature = "gzip"))]
unsafe fn write_back_inflate_header(strm: z_streamp) {
    // Look up the caller's registered `gz_header` for this stream; absent (or
    // null) registration means there is nothing to write back.
    let head = match header_reg::lookup(strm) {
        Some(h) if !h.is_null() => h,
        _ => return,
    };
    // SAFETY: `strm` was just driven by `run_inflate`; re-validate its state.
    let snapshot = match unsafe { stream_state(strm) } {
        Some(z) => match z.inflate_state() {
            Some(s) => s.header().map(HeaderSnapshot::capture),
            None => None,
        },
        None => None,
    };
    if let Some(snap) = snapshot {
        // SAFETY: `head` is the caller's registered, valid `gz_header` pointer;
        // `write_to` clamps every copy to the caller-provided `*_max` capacities.
        unsafe { snap.write_to(head) };
    }
}

/// C `inflateInit2_` — allocate and initialize an inflate stream with explicit
/// `windowBits` (honoring zlib/raw/gzip/auto overloading, which the core
/// decodes — the boundary passes the raw value through unchanged).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn inflateInit2_(
    strm: z_streamp,
    windowBits: c_int,
    version: *const c_char,
    stream_size: c_int,
) -> c_int {
    // SAFETY: `version` is the caller-supplied NUL-terminated version string.
    if let Err(code) = unsafe { check_version(version, stream_size) } {
        return code;
    }
    if strm.is_null() {
        return Z_STREAM_ERROR;
    }
    // Honor caller-installed custom allocation: a non-null `zalloc` builds an
    // adapter (shared via `Rc` so the state stays `Clone` for `inflateCopy`)
    // that routes the lazily-allocated sliding window through
    // `zalloc`/`zfree`/`opaque`; a window allocation failure then surfaces as
    // `Z_MEM_ERROR`, exactly like C `updatewindow`.
    // SAFETY: `strm` is non-null (checked above); reading the C-ABI
    // `zalloc`/`zfree`/`opaque` fields is valid per the `z_stream` contract.
    let (zalloc, zfree, opaque) = unsafe { ((*strm).zalloc, (*strm).zfree, (*strm).opaque) };
    let allocator = translate::CAllocator::from_callbacks(zalloc, zfree, opaque)
        .map(|adapter| Rc::new(adapter) as Rc<dyn zlib_rs::stream::Allocator>);
    match zlib_rs::inflate::InflateState::new_in(windowBits, allocator) {
        Ok(inner) => {
            // Capture the post-reset wrap so we can publish the initial adler,
            // mirroring C `inflateInit -> inflateReset: strm->adler = wrap & 1`.
            let initial_adler = (inner.wrap & 1) as c_ulong;
            let mut z = Box::new(ZStream::new());
            z.set_inflate_state(inner);
            // SAFETY: `strm` is non-null (checked); transfer ownership of the
            // freshly-initialized state into the C `state` field.
            unsafe { attach_state(strm, z) };
            // SAFETY: `strm` is non-null; reset the status scalars as C does.
            unsafe {
                (*strm).msg = core::ptr::null();
                (*strm).adler = initial_adler;
                (*strm).total_in = 0;
                (*strm).total_out = 0;
            }
            Z_OK
        }
        Err(e) => e.as_i32(),
    }
}

/// C `inflateInit_` — `inflateInit2_` with the default window size
/// (`MAX_WBITS` = 15, i.e. a zlib-wrapped 32 KiB window).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn inflateInit_(
    strm: z_streamp,
    version: *const c_char,
    stream_size: c_int,
) -> c_int {
    // SAFETY: forwards to `inflateInit2_`, which performs all validation.
    unsafe { inflateInit2_(strm, zlib_rs::constants::MAX_WBITS, version, stream_size) }
}

/// C `inflate` — run one decompression step, then (std) write back any
/// requested gzip header fields into the caller's registered `gz_header`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn inflate(strm: z_streamp, flush: c_int) -> c_int {
    // SAFETY: caller-supplied z_streamp; `run_inflate` validates null/state and
    // forms the input/output slices from `next_in`/`next_out`, writing back the
    // consumed/produced cursors, totals, adler, msg and data_type.
    let rc = unsafe { translate::run_inflate(strm, flush) };
    // The gzip-header write-back exists only when both `std` (the registry) and
    // `gzip` (the core's header capture) are present; std-only / no_std builds
    // skip it (the core still decodes correctly, it just cannot fill a C
    // `gz_header`).
    #[cfg(all(feature = "std", feature = "gzip"))]
    {
        // SAFETY: `strm` is the same pointer just driven by `run_inflate`.
        unsafe { write_back_inflate_header(strm) };
    }
    rc
}

/// C `inflateEnd` — free the inflate state (RAII drop of the owning box) and
/// forget any gzip-header registration for this stream.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn inflateEnd(strm: z_streamp) -> c_int {
    // SAFETY: caller-supplied z_streamp; `stream_state` checks null/state.
    let z = match unsafe { stream_state(strm) } {
        Some(z) => z,
        None => return Z_STREAM_ERROR,
    };
    if !z.is_inflate() {
        // Not an inflate stream: leave the inner state untouched (the caller
        // will use `deflateEnd`); matches C returning Z_STREAM_ERROR.
        return Z_STREAM_ERROR;
    }
    // Drop the OUTER `Box<ZStream>`; its `Drop` frees the inner inflate state
    // (RAII == C `inflateEnd`).
    // SAFETY: `strm` owns the outer box; `drop_state` reconstructs/drops once.
    unsafe { translate::drop_state(strm) };
    // Drop any gzip-header registration (present only under `std` + `gzip`).
    #[cfg(all(feature = "std", feature = "gzip"))]
    header_reg::unregister(strm);
    Z_OK
}

/// C `inflateReset` — reset the stream to a fresh post-init state, preserving
/// the allocated window and `windowBits`. Zeros the C-visible counters and
/// republishes the initial adler (`wrap & 1`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn inflateReset(strm: z_streamp) -> c_int {
    // SAFETY: caller-supplied z_streamp; `stream_state` checks null/state.
    let z = match unsafe { stream_state(strm) } {
        Some(z) => z,
        None => return Z_STREAM_ERROR,
    };
    let result;
    let adler;
    {
        let s = match z.inflate_state_mut() {
            Some(s) => s,
            None => return Z_STREAM_ERROR,
        };
        result = s.reset();
        adler = (s.wrap & 1) as c_ulong;
    }
    // The core `reset()` zeros only the InflateState's internal counters; zero
    // the (Rust) ZStream counters too so the two stay consistent.
    z.reset_counters();
    // SAFETY: `strm` is valid (state recovered above); mirror the reset scalars
    // onto the C struct (C `inflateResetKeep`: total_in/out = 0, msg = NULL,
    // adler = wrap & 1).
    unsafe {
        (*strm).total_in = 0;
        (*strm).total_out = 0;
        (*strm).msg = core::ptr::null();
        (*strm).adler = adler;
    }
    to_c_int(Ok(result))
}

/// C `inflateReset2` — like `inflateReset` but also reselect the wrap/window
/// from a new `windowBits` (zlib/raw/gzip/auto overloading decoded by the core).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn inflateReset2(strm: z_streamp, windowBits: c_int) -> c_int {
    // SAFETY: caller-supplied z_streamp; `stream_state` checks null/state.
    let z = match unsafe { stream_state(strm) } {
        Some(z) => z,
        None => return Z_STREAM_ERROR,
    };
    let adler;
    {
        let s = match z.inflate_state_mut() {
            Some(s) => s,
            None => return Z_STREAM_ERROR,
        };
        match s.reset2(windowBits) {
            Ok(_) => adler = (s.wrap & 1) as c_ulong,
            Err(e) => return e.as_i32(),
        }
    }
    z.reset_counters();
    // SAFETY: `strm` is valid (state recovered above); mirror reset scalars.
    unsafe {
        (*strm).total_in = 0;
        (*strm).total_out = 0;
        (*strm).msg = core::ptr::null();
        (*strm).adler = adler;
    }
    Z_OK
}

/// C `inflateSync` — skip forward to the next possible full flush point to
/// recover from corrupted input, advancing the C input cursor by what the
/// byte-resync consumed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn inflateSync(strm: z_streamp) -> c_int {
    // SAFETY: caller-supplied z_streamp; `stream_state` checks null/state.
    let z = match unsafe { stream_state(strm) } {
        Some(z) => z,
        None => return Z_STREAM_ERROR,
    };
    // SAFETY: `next_in`/`avail_in` describe the caller's input buffer.
    let (next_in, avail_in) = unsafe { ((*strm).next_in, (*strm).avail_in) };
    let consumed;
    let result;
    {
        let s = match z.inflate_state_mut() {
            Some(s) => s,
            None => return Z_STREAM_ERROR,
        };
        // SAFETY: input slice formed from the caller's (next_in, avail_in).
        let input = unsafe { input_slice(next_in, avail_in) };
        let (c, r) = s.sync(input);
        consumed = c;
        result = r;
    }
    // Advance the C input cursor by the bytes the resync consumed.
    // SAFETY: `strm` is valid; `consumed <= avail_in` (the core never
    // over-consumes its input slice).
    unsafe {
        if consumed != 0 {
            (*strm).next_in = (*strm).next_in.add(consumed);
        }
        (*strm).avail_in -= consumed as c_uint;
        (*strm).total_in = (*strm).total_in.wrapping_add(consumed as c_ulong);
    }
    to_c_int(result)
}

/// C `inflateCopy` — deep-copy an inflate stream (state + window) into a fresh
/// destination stream.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn inflateCopy(dest: z_streamp, source: z_streamp) -> c_int {
    if dest.is_null() {
        return Z_STREAM_ERROR;
    }
    // SAFETY: caller-supplied source z_streamp; `stream_state` checks null/state.
    let src = match unsafe { stream_state(source) } {
        Some(z) => z,
        None => return Z_STREAM_ERROR,
    };
    let copied = match src.inflate_state() {
        Some(s) => s.copy(),
        None => return Z_STREAM_ERROR,
    };
    let mut dz = Box::new(ZStream::new());
    dz.set_inflate_state(copied);
    // SAFETY: `dest` is non-null (checked); transfer ownership of the copy.
    unsafe { attach_state(dest, dz) };
    // SAFETY: both `dest` and `source` are valid z_stream pointers; copy the
    // public scalar fields (next_in/out, avail, totals, adler, data_type).
    unsafe { copy_stream_scalars(dest, source) };
    Z_OK
}

/// C `inflatePrime` — insert `bits` (0..=16) bits of `value` ahead of the input
/// stream (used to resume mid-byte, e.g. after `inflateBack`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn inflatePrime(strm: z_streamp, bits: c_int, value: c_int) -> c_int {
    // SAFETY: caller-supplied z_streamp; `stream_state` checks null/state.
    let z = match unsafe { stream_state(strm) } {
        Some(z) => z,
        None => return Z_STREAM_ERROR,
    };
    let s = match z.inflate_state_mut() {
        Some(s) => s,
        None => return Z_STREAM_ERROR,
    };
    to_c_int(s.prime(bits, value))
}

/// C `inflateMark` — return the bit/byte position packed as a `long` (high
/// bits = bytes consumed of the current block, low 16 bits = bits remaining in
/// the bit accumulator). Returns `-(1 << 16)` for an invalid state.
///
/// NOTE: the return type is `c_long`, NOT `c_int` — this is a deliberate
/// `zlib.h` quirk preserved for byte-for-byte ABI parity.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn inflateMark(strm: z_streamp) -> c_long {
    // C returns -(1L << 16) when the state cannot be inspected.
    const INVALID: c_long = -(1 << 16);
    // SAFETY: caller-supplied z_streamp; `stream_state` checks null/state.
    match unsafe { stream_state(strm) } {
        Some(z) => match z.inflate_state() {
            Some(s) => s.mark() as c_long,
            None => INVALID,
        },
        None => INVALID,
    }
}

/// C `inflateGetHeader` — request that the gzip header from a gzip-wrapped
/// stream be captured and written into the caller's `gz_header` as `inflate`
/// parses it.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn inflateGetHeader(strm: z_streamp, head: gz_headerp) -> c_int {
    // SAFETY: caller-supplied z_streamp; `stream_state` checks null/state.
    let z = match unsafe { stream_state(strm) } {
        Some(z) => z,
        None => return Z_STREAM_ERROR,
    };
    let s = match z.inflate_state_mut() {
        Some(s) => s,
        None => return Z_STREAM_ERROR,
    };
    // Gzip-header capture exists in the core only under the `gzip` feature
    // (`InflateState::get_header` is `#[cfg(feature = "gzip")]`). With `gzip`
    // disabled no inflate stream can be gzip-wrapped, so — exactly as C
    // `inflateGetHeader` reports for a non-gzip stream — return `Z_STREAM_ERROR`.
    #[cfg(feature = "gzip")]
    {
        // Build a capture-enabled core header from the caller's `gz_header`.
        // CRITICAL: the core only stores parsed name/comment/extra bytes into
        // fields that are `Some(_)`; a plain `GzHeader::new()` (all `None`) makes
        // `inflate` consume and DISCARD those header bytes, leaving the caller's
        // buffers empty (the defect this fixes). `gzheader_capture_from_c`
        // therefore opts each field into capture exactly when the caller supplied
        // a non-null buffer with positive capacity, mirroring the C contract.
        let core_head = if head.is_null() {
            GzHeader::new()
        } else {
            // SAFETY: `head` is non-null and, per the C `inflateGetHeader`
            // contract, a valid readable `gz_header`.
            unsafe { gzheader_capture_from_c(head) }
        };
        // Enable capture in the core; it returns Err(StreamError) unless the
        // stream is gzip-wrapped (wrap & 2), exactly as C `inflateGetHeader` does.
        match s.get_header(core_head) {
            Ok(_) => {
                if !head.is_null() {
                    // Mirror C `inflateGetHeader`, which sets `head->done = 0`
                    // immediately on success so a caller inspecting `done` before
                    // the first `inflate` observes the not-yet-complete state.
                    // SAFETY: `head` is non-null and a valid `gz_header`; `done`
                    // is an in-bounds field write.
                    unsafe { (*head).done = 0 };
                }
                // Record the caller's C `gz_header` so `inflate` can write the
                // parsed fields back into it (std-only; under no_std the core
                // still captures the header but the C write-back is skipped).
                #[cfg(feature = "std")]
                header_reg::register(strm, head);
                Z_OK
            }
            Err(e) => e.as_i32(),
        }
    }
    #[cfg(not(feature = "gzip"))]
    {
        let _ = (s, head);
        Z_STREAM_ERROR
    }
}

/// C `inflateSetDictionary` — set the preset dictionary used to decompress a
/// raw stream (or one reporting `Z_NEED_DICT`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn inflateSetDictionary(
    strm: z_streamp,
    dictionary: *const u8,
    dictLength: c_uint,
) -> c_int {
    // SAFETY: caller-supplied z_streamp; `stream_state` checks null/state.
    let z = match unsafe { stream_state(strm) } {
        Some(z) => z,
        None => return Z_STREAM_ERROR,
    };
    let s = match z.inflate_state_mut() {
        Some(s) => s,
        None => return Z_STREAM_ERROR,
    };
    // SAFETY: `(dictionary, dictLength)` describe the caller's dictionary buffer.
    let dict = unsafe { input_slice(dictionary, dictLength) };
    to_c_int(s.set_dictionary(dict))
}

/// C `inflateGetDictionary` — copy the sliding-window dictionary out to the
/// caller. Queries the length first (null buffer), then copies when a non-null
/// `dictionary` is supplied; writes the length back through `dictLength`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn inflateGetDictionary(
    strm: z_streamp,
    dictionary: *mut u8,
    dictLength: *mut c_uint,
) -> c_int {
    // SAFETY: caller-supplied z_streamp; `stream_state` checks null/state.
    let z = match unsafe { stream_state(strm) } {
        Some(z) => z,
        None => return Z_STREAM_ERROR,
    };
    let s = match z.inflate_state() {
        Some(s) => s,
        None => return Z_STREAM_ERROR,
    };
    let dlen = s.get_dictionary(None);
    if !dictionary.is_null() && dlen > 0 {
        // SAFETY: the caller guarantees `dictionary` points to a buffer of at
        // least the current dictionary length (`dlen`) bytes.
        let buf = unsafe { output_slice(dictionary, dlen as c_uint) };
        let _ = s.get_dictionary(Some(buf));
    }
    if !dictLength.is_null() {
        // SAFETY: `dictLength` is a valid writable `c_uint` pointer (guarded).
        unsafe { *dictLength = dlen as c_uint };
    }
    Z_OK
}

/// C `inflateBackInit_` — initialize a stream for the callback-driven
/// `inflateBack` decoder. The caller's `window` pointer must be non-null (per
/// the C contract) but is used only as internal scratch in C; our core owns its
/// own `1 << windowBits` window, which is behaviorally equivalent.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn inflateBackInit_(
    strm: z_streamp,
    windowBits: c_int,
    window: *mut u8,
    version: *const c_char,
    stream_size: c_int,
) -> c_int {
    // SAFETY: `version` is the caller-supplied NUL-terminated version string.
    if let Err(code) = unsafe { check_version(version, stream_size) } {
        return code;
    }
    // C requires both `strm` and the caller's `window` buffer to be non-null.
    if strm.is_null() || window.is_null() {
        return Z_STREAM_ERROR;
    }
    match zlib_rs::inflate::inflate_back_init(windowBits) {
        Ok(inner) => {
            let mut z = Box::new(ZStream::new());
            z.set_inflate_state(inner);
            // SAFETY: `strm` is non-null (checked); transfer ownership.
            unsafe { attach_state(strm, z) };
            // SAFETY: `strm` is non-null; clear any stale error message.
            unsafe { (*strm).msg = core::ptr::null() };
            Z_OK
        }
        Err(e) => e.as_i32(),
    }
}

/// C `inflateBack` — decompress a raw DEFLATE stream using caller-supplied
/// `in()` / `out()` callbacks instead of `next_in`/`next_out`. The single most
/// pointer-heavy inflate symbol: it bridges the two C callbacks to the core's
/// `FnMut() -> &[u8]` / `FnMut(&[u8]) -> bool` closures.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn inflateBack(
    strm: z_streamp,
    in_: in_func,
    in_desc: *mut c_void,
    out: out_func,
    out_desc: *mut c_void,
) -> c_int {
    // SAFETY: caller-supplied z_streamp; `stream_state` checks null/state.
    let z = match unsafe { stream_state(strm) } {
        Some(z) => z,
        None => return Z_STREAM_ERROR,
    };
    let s = match z.inflate_state_mut() {
        Some(s) => s,
        None => return Z_STREAM_ERROR,
    };

    // Bridge the C `in()` callback to the core's `FnMut() -> &[u8]`. An empty
    // slice signals end-of-input (the core then yields Z_BUF_ERROR).
    let read = || -> &[u8] {
        let Some(in_fn) = in_ else {
            return &[];
        };
        let mut buf: *const u8 = core::ptr::null();
        // SAFETY: `in_fn`/`in_desc` are the caller's `inflateBack` input
        // callback and descriptor; `&mut buf` is a valid out-pointer.
        let n = unsafe { in_fn(in_desc, &mut buf) };
        if n == 0 || buf.is_null() {
            return &[];
        }
        // SAFETY: the callback contract guarantees `buf` points to `n` readable
        // bytes valid until the next `in()` call (or the end of `inflateBack`).
        unsafe { core::slice::from_raw_parts(buf, n as usize) }
    };

    // Bridge the C `out()` callback to the core's `FnMut(&[u8]) -> bool`. The
    // core treats a `true` return as FAILURE; C `out()` returns non-zero on
    // failure, so the mapping is `failed = (rc != 0)`.
    let write = |data: &[u8]| -> bool {
        let Some(out_fn) = out else {
            return true;
        };
        // SAFETY: `out_fn`/`out_desc` are the caller's `inflateBack` output
        // callback and descriptor; `data` is valid for `data.len()` bytes.
        let rc = unsafe { out_fn(out_desc, data.as_ptr() as *mut u8, data.len() as c_uint) };
        rc != 0
    };

    let result = zlib_rs::inflate::inflate_back(s, read, write);

    // Hand back the unconsumed input tail through `next_in`/`avail_in`, as C does.
    let unused = result.unused_input;
    // SAFETY: `strm` is valid; `unused` points into the caller's last input
    // chunk (still owned by the caller) and its length fits a `c_uint`.
    unsafe {
        (*strm).next_in = unused.as_ptr();
        (*strm).avail_in = unused.len() as c_uint;
    }
    to_c_int(result.status)
}

/// C `inflateBackEnd` — free an `inflateBack` stream. Equivalent to dropping the
/// owning box (RAII); there is no status beyond `Z_OK` on a valid state.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn inflateBackEnd(strm: z_streamp) -> c_int {
    // SAFETY: caller-supplied z_streamp; `stream_state` checks null/state.
    let z = match unsafe { stream_state(strm) } {
        Some(z) => z,
        None => return Z_STREAM_ERROR,
    };
    if !z.is_inflate() {
        return Z_STREAM_ERROR;
    }
    // The core's `inflate_back_end` consumes a `Box<InflateState>`; our state is
    // owned by the outer `Box<ZStream>`, so dropping the outer box frees it
    // (RAII == C `inflateBackEnd`).
    // SAFETY: `strm` owns the outer box; `drop_state` reconstructs/drops once.
    unsafe { translate::drop_state(strm) };
    Z_OK
}

// ===========================================================================
// AAP-Phase 3 — one-shot helpers (5 symbols)
//
// `compress`/`uncompress` operate on whole buffers in a single call. The C
// `destLen` is an in/out parameter: on entry it is the destination capacity, on
// return it is the produced length. `uncompress2`'s `sourceLen` is likewise
// in/out (entry = available source, return = bytes consumed). Slice formation
// and the length read/write-back go through `translate`; the produced/consumed
// arithmetic and the `Z_BUF_ERROR`/`Z_DATA_ERROR` mapping live in the core.
// ===========================================================================

/// C `compress2` — one-shot compress `source` into `dest` at the given `level`.
/// `destLen` is in/out (capacity in, produced out).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn compress2(
    dest: *mut u8,
    destLen: *mut c_ulong,
    source: *const u8,
    sourceLen: c_ulong,
    level: c_int,
) -> c_int {
    if destLen.is_null() {
        return Z_STREAM_ERROR;
    }
    // SAFETY: `destLen` is non-null (checked); it is the in/out capacity word.
    let cap = unsafe { translate::read_dest_len(destLen) };
    // Canonical-zlib argument validation (compress.c `compress2_z`): a NULL
    // source with a positive length, or a NULL destination with a positive
    // capacity, is a usage error and MUST return Z_STREAM_ERROR — exactly as C
    // zlib does (its inner `deflate` rejects `next_in == NULL && avail_in != 0`
    // / `next_out == NULL`). Without this guard the shim would turn the NULL
    // pointer into an empty slice and silently succeed (produce an 8-byte empty
    // zlib stream), masking the caller's bug. A NULL pointer paired with a zero
    // length stays legal (it becomes an empty slice below).
    if (sourceLen > 0 && source.is_null()) || (cap > 0 && dest.is_null()) {
        return Z_STREAM_ERROR;
    }
    // SAFETY: `dest` is valid for `cap` bytes (its declared capacity) or null
    // (then an empty slice), and `source` is valid for `sourceLen` bytes or null.
    let dst = unsafe { translate::output_slice_sz(dest, cap as size_t) };
    let src = unsafe { translate::input_slice_sz(source, sourceLen as size_t) };
    match zlib_rs::util::compress2(dst, src, level) {
        Ok(produced) => {
            // SAFETY: `destLen` is non-null and writable (checked above).
            unsafe { translate::write_dest_len(destLen, produced) };
            Z_OK
        }
        Err(e) => e.as_i32(),
    }
}

/// C `compress` — `compress2` at the default compression level.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn compress(
    dest: *mut u8,
    destLen: *mut c_ulong,
    source: *const u8,
    sourceLen: c_ulong,
) -> c_int {
    // SAFETY: forwards to `compress2`, which performs all validation.
    unsafe { compress2(dest, destLen, source, sourceLen, Z_DEFAULT_COMPRESSION) }
}

/// C `compressBound` — upper bound on the compressed size of `sourceLen` bytes.
/// Delegates to the core's `compress_bound` (which reproduces the zlib formula).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn compressBound(sourceLen: c_ulong) -> c_ulong {
    zlib_rs::util::compress_bound(sourceLen as usize) as c_ulong
}

/// C `uncompress` — one-shot decompress `source` into `dest`. `destLen` is
/// in/out (capacity in, produced out).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn uncompress(
    dest: *mut u8,
    destLen: *mut c_ulong,
    source: *const u8,
    sourceLen: c_ulong,
) -> c_int {
    if destLen.is_null() {
        return Z_STREAM_ERROR;
    }
    // SAFETY: `destLen` is non-null (checked); the in/out capacity word.
    let cap = unsafe { translate::read_dest_len(destLen) };
    // Canonical-zlib argument validation (uncompr.c): reject a NULL source with
    // a positive length or a NULL destination with a positive capacity with
    // Z_STREAM_ERROR, matching C zlib's inner `inflate` NULL guards. (A NULL
    // pointer with a zero length remains legal — an empty slice below.)
    if (sourceLen > 0 && source.is_null()) || (cap > 0 && dest.is_null()) {
        return Z_STREAM_ERROR;
    }
    // SAFETY: `dest` valid for `cap` bytes or null; `source` valid for
    // `sourceLen` bytes or null (both yield empty slices when null).
    let dst = unsafe { translate::output_slice_sz(dest, cap as size_t) };
    let src = unsafe { translate::input_slice_sz(source, sourceLen as size_t) };
    match zlib_rs::util::uncompress(dst, src) {
        Ok(produced) => {
            // SAFETY: `destLen` is non-null and writable (checked above).
            unsafe { translate::write_dest_len(destLen, produced) };
            Z_OK
        }
        Err(e) => e.as_i32(),
    }
}

/// C `uncompress2` — like `uncompress`, but `sourceLen` is in/out: on return it
/// holds the number of source bytes actually consumed (the core reports the
/// consumed count alongside the produced count).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn uncompress2(
    dest: *mut u8,
    destLen: *mut c_ulong,
    source: *const u8,
    sourceLen: *mut c_ulong,
) -> c_int {
    if destLen.is_null() || sourceLen.is_null() {
        return Z_STREAM_ERROR;
    }
    // SAFETY: `destLen`/`sourceLen` are non-null (checked); both are in/out words.
    let cap = unsafe { translate::read_dest_len(destLen) };
    // SAFETY: `sourceLen` is non-null (checked); read the available source size.
    let src_len = unsafe { *sourceLen as size_t };
    // Canonical-zlib argument validation (uncompr.c `uncompress2_z`): a NULL
    // source with a positive `*sourceLen`, or a NULL destination with a positive
    // capacity, is Z_STREAM_ERROR (the NULL `sourceLen`/`destLen` cases are
    // already handled above). Matches C zlib exactly. (NULL + zero length stays
    // legal — an empty slice below.)
    if (src_len > 0 && source.is_null()) || (cap > 0 && dest.is_null()) {
        return Z_STREAM_ERROR;
    }
    // SAFETY: `dest` valid for `cap` bytes or null; `source` valid for `src_len`
    // bytes or null.
    let dst = unsafe { translate::output_slice_sz(dest, cap as size_t) };
    let src = unsafe { translate::input_slice_sz(source, src_len) };
    match zlib_rs::util::uncompress2(dst, src) {
        Ok((produced, consumed)) => {
            // SAFETY: both out-pointers are non-null and writable (checked).
            unsafe {
                translate::write_dest_len(destLen, produced);
                *sourceLen = consumed as c_ulong;
            }
            Z_OK
        }
        Err(e) => e.as_i32(),
    }
}

// ===========================================================================
// AAP-Phase 4 — checksums (9 symbols)
//
// All numeric conversions (c_ulong ⇄ u32 masking, the NULL-buf rule
// [adler32→1, crc32→0], and signed off_t → unsigned length) live in the
// `translate` checksum helpers; these exports are pure delegation.
// ===========================================================================

/// C `adler32` — update a running Adler-32 with `(buf, len)`. A NULL `buf`
/// returns the initial value `1`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn adler32(adler: c_ulong, buf: *const u8, len: c_uint) -> c_ulong {
    // SAFETY: `(buf, len)` describe the caller's input buffer (or NULL/0).
    unsafe { translate::adler32_ffi(adler, buf, len) }
}

/// C `adler32_z` — `adler32` with a `size_t` length (large-buffer variant).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn adler32_z(adler: c_ulong, buf: *const u8, len: size_t) -> c_ulong {
    // SAFETY: `(buf, len)` describe the caller's input buffer (or NULL/0).
    unsafe { translate::adler32_z_ffi(adler, buf, len) }
}

/// C `adler32_combine` — combine two Adler-32 values as if the two blocks (the
/// second of length `len2`) had been checksummed in sequence.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn adler32_combine(adler1: c_ulong, adler2: c_ulong, len2: off_t) -> c_ulong {
    translate::adler32_combine_ffi(adler1, adler2, len2)
}

/// C `crc32` — update a running CRC-32 (IEEE) with `(buf, len)`. A NULL `buf`
/// returns the initial value `0`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn crc32(crc: c_ulong, buf: *const u8, len: c_uint) -> c_ulong {
    // SAFETY: `(buf, len)` describe the caller's input buffer (or NULL/0).
    unsafe { translate::crc32_ffi(crc, buf, len) }
}

/// C `crc32_z` — `crc32` with a `size_t` length (large-buffer variant).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn crc32_z(crc: c_ulong, buf: *const u8, len: size_t) -> c_ulong {
    // SAFETY: `(buf, len)` describe the caller's input buffer (or NULL/0).
    unsafe { translate::crc32_z_ffi(crc, buf, len) }
}

/// C `crc32_combine` — combine two CRC-32 values as if the two blocks (the
/// second of length `len2`) had been CRC'd in sequence.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn crc32_combine(crc1: c_ulong, crc2: c_ulong, len2: off_t) -> c_ulong {
    translate::crc32_combine_ffi(crc1, crc2, len2)
}

/// C `crc32_combine_gen` — precompute the combine operator for a second block
/// of length `len2`, for repeated use with `crc32_combine_op`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn crc32_combine_gen(len2: off_t) -> c_ulong {
    translate::crc32_combine_gen_ffi(len2)
}

/// C `crc32_combine_op` — combine two CRC-32 values using a precomputed operator
/// `op` (from `crc32_combine_gen`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn crc32_combine_op(crc1: c_ulong, crc2: c_ulong, op: c_ulong) -> c_ulong {
    translate::crc32_combine_op_ffi(crc1, crc2, op)
}

/// C `get_crc_table` — return a pointer to the 256-entry CRC-32 lookup table
/// (`z_crc_t` = `u32`), as some legacy consumers expect.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn get_crc_table() -> *const z_crc_t {
    translate::get_crc_table_ptr()
}

// ===========================================================================
// AAP-Phase 5 — version / diagnostics (3 symbols)
// ===========================================================================

/// C `zlibVersion` — return the runtime ABI version string. This MUST be
/// exactly `"1.3.2.1-motley"` (the C `ZLIB_VERSION` define), independent of the
/// crate's Cargo semver.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn zlibVersion() -> *const c_char {
    version_ptr()
}

/// C `zlibCompileFlags` — return the bitfield describing the compile-time type
/// sizes and options (computed by the core to match the running platform's
/// `uInt`/`uLong`/`voidpf`/`z_off_t` widths).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn zlibCompileFlags() -> c_ulong {
    zlib_compile_flags()
}

/// C `zError` — map a zlib error code to its `'static` NUL-terminated message
/// string (backs the core's `err_msg` table).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn zError(err: c_int) -> *const c_char {
    z_error_cstr(err)
}

// ===========================================================================
// AAP-Phase 6 — gz file API (26 symbols) — gated behind `gz-io` (default ON)
//
// The C `gzFile` is a semi-opaque `*mut gzFile_s` whose first three fields
// (`have`, `next`, `pos`) the `gzgetc` MACRO in the generated header reads
// directly. We therefore return a pointer to `GzFileShim`, a `#[repr(C)]`
// struct whose first three fields are layout-compatible with `gzFile_s`,
// followed by the owned safe-core `GzFile` and a NUL-terminated error mirror.
//
// We keep `have == 0` permanently so the `gzgetc` macro ALWAYS takes its
// fallback branch — `(gzgetc)(g)`, i.e. the exported `gzgetc` function below —
// instead of reading bytes out of the inline buffer. This means the inline
// `have`/`next`/`pos` header need never be kept in sync with the core's
// internal read-ahead buffer; all real work stays in the safe core.
//
// `translate` supplies the generic, state-type-agnostic helpers
// (`gzfile_from_state` / `gz_state_mut` / `drop_gzfile` / `file_from_fd` /
// `i64_to_off_t` / `off_t_to_i64` / `gz_write_fmt`); this module supplies the
// concrete `GzFileShim` whose layout upholds their contract.
// ===========================================================================

#[cfg(feature = "gz-io")]
use core::ffi::CStr;
#[cfg(feature = "gz-io")]
use zlib_rs::gz::GzFile;

/// The concrete gz state behind a C `gzFile`. The first three fields are
/// layout-compatible with `gzFile_s` (`have`, `next`, `pos`) so the generated
/// `gzgetc` macro can read them; they are held at `have = 0` / `next = NULL`
/// so the macro always defers to the exported `gzgetc` fallback. `inner` owns
/// the safe-core file; `err_buf` keeps a NUL-terminated copy of the last error
/// message so `gzerror` can return a stable `*const c_char`.
#[cfg(feature = "gz-io")]
#[repr(C)]
struct GzFileShim {
    /// `unsigned have` — kept 0 so the `gzgetc` macro defers to the fallback.
    have: c_uint,
    /// `unsigned char *next` — kept null (the macro never dereferences it).
    next: *mut u8,
    /// `z_off64_t pos` — vestigial; positions are tracked by the core.
    pos: z_off64_t,
    /// The owned safe-core gz file handle (does the real work).
    inner: GzFile,
    /// NUL-terminated mirror of the last `gzerror` message (stable pointer).
    err_buf: Vec<u8>,
}

/// Box a freshly-opened core [`GzFile`] into a [`GzFileShim`] and hand back the
/// C `gzFile` view of it (layout head = `gzFile_s`).
#[cfg(feature = "gz-io")]
fn new_gzfile_shim(inner: GzFile) -> gzFile {
    let shim = Box::new(GzFileShim {
        have: 0,
        next: core::ptr::null_mut(),
        pos: 0,
        inner,
        err_buf: Vec::new(),
    });
    translate::gzfile_from_state(shim)
}

/// Convert a borrowed C path string to an owned `PathBuf`. On Unix the raw
/// bytes are preserved verbatim (paths are not required to be UTF-8); elsewhere
/// a lossy UTF-8 interpretation is used.
#[cfg(all(feature = "gz-io", unix))]
fn cstr_to_path(c: &CStr) -> std::path::PathBuf {
    use std::os::unix::ffi::OsStrExt;
    std::path::PathBuf::from(std::ffi::OsStr::from_bytes(c.to_bytes()))
}

/// Non-Unix counterpart of [`cstr_to_path`] using a lossy UTF-8 conversion.
#[cfg(all(feature = "gz-io", not(unix)))]
fn cstr_to_path(c: &CStr) -> std::path::PathBuf {
    std::path::PathBuf::from(c.to_string_lossy().into_owned())
}

/// C `gzopen` — open `path` for gzip reading/writing per `mode` (e.g. `"rb"`,
/// `"wb9"`). Returns NULL on failure.
#[cfg(feature = "gz-io")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzopen(path: *const c_char, mode: *const c_char) -> gzFile {
    if path.is_null() || mode.is_null() {
        return core::ptr::null_mut();
    }
    // SAFETY: `path`/`mode` are caller-provided NUL-terminated C strings.
    let path_buf = cstr_to_path(unsafe { CStr::from_ptr(path) });
    let mode_str = match unsafe { CStr::from_ptr(mode) }.to_str() {
        Ok(s) => s,
        Err(_) => return core::ptr::null_mut(),
    };
    match zlib_rs::gz::gzopen(&path_buf, mode_str) {
        Some(inner) => new_gzfile_shim(inner),
        None => core::ptr::null_mut(),
    }
}

/// C `gzdopen` — wrap an already-open OS file descriptor `fd` (ownership is
/// transferred, as in C) for gzip I/O per `mode`. Returns NULL on failure.
#[cfg(feature = "gz-io")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzdopen(fd: c_int, mode: *const c_char) -> gzFile {
    if mode.is_null() {
        return core::ptr::null_mut();
    }
    // SAFETY: `mode` is a caller-provided NUL-terminated C string (null-checked).
    let mode_str = match unsafe { CStr::from_ptr(mode) }.to_str() {
        Ok(s) => s,
        Err(_) => return core::ptr::null_mut(),
    };
    // SAFETY: C `gzdopen` transfers ownership of `fd` to the new handle.
    let file = unsafe { translate::file_from_fd(fd) };
    match zlib_rs::gz::gzdopen(file, fd, mode_str) {
        Some(inner) => new_gzfile_shim(inner),
        None => core::ptr::null_mut(),
    }
}

/// C `gzbuffer` — set the internal buffer size for `file` (before the first
/// read/write). Returns 0 on success, -1 on error.
#[cfg(feature = "gz-io")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzbuffer(file: gzFile, size: c_uint) -> c_int {
    // SAFETY: `file` is a handle from `new_gzfile_shim` (or null, guarded).
    let shim = match unsafe { translate::gz_state_mut::<GzFileShim>(file) } {
        Some(s) => s,
        None => return -1,
    };
    zlib_rs::gz::gzbuffer(&mut shim.inner, size)
}

/// C `gzsetparams` — change the compression `level`/`strategy` mid-stream.
#[cfg(feature = "gz-io")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzsetparams(file: gzFile, level: c_int, strategy: c_int) -> c_int {
    // SAFETY: `file` is a handle from `new_gzfile_shim` (or null, guarded).
    let shim = match unsafe { translate::gz_state_mut::<GzFileShim>(file) } {
        Some(s) => s,
        None => return Z_STREAM_ERROR,
    };
    zlib_rs::gz::gzsetparams(&mut shim.inner, level, strategy)
}

/// C `gzread` — read up to `len` uncompressed bytes into `buf`. Returns the
/// number of bytes read (0 at EOF) or -1 on error.
#[cfg(feature = "gz-io")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzread(file: gzFile, buf: *mut c_void, len: c_uint) -> c_int {
    // SAFETY: `file` is a handle from `new_gzfile_shim` (or null, guarded).
    let shim = match unsafe { translate::gz_state_mut::<GzFileShim>(file) } {
        Some(s) => s,
        None => return -1,
    };
    // SAFETY: `(buf, len)` describe the caller's writable output buffer.
    let out = unsafe { translate::output_slice(buf as *mut u8, len) };
    zlib_rs::gz::gzread(&mut shim.inner, out)
}

/// C `gzfread` — read `nitems` items of `size` bytes each into `buf`. Returns
/// the number of full items read. Guards against `size * nitems` overflow
/// (returns 0, as C does).
#[cfg(feature = "gz-io")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzfread(
    buf: *mut c_void,
    size: size_t,
    nitems: size_t,
    file: gzFile,
) -> size_t {
    // SAFETY: `file` is a handle from `new_gzfile_shim` (or null, guarded).
    let shim = match unsafe { translate::gz_state_mut::<GzFileShim>(file) } {
        Some(s) => s,
        None => return 0,
    };
    let total = match size.checked_mul(nitems) {
        Some(t) => t,
        None => return 0,
    };
    // SAFETY: `(buf, total)` describe the caller's writable output buffer.
    let out = unsafe { translate::output_slice_sz(buf as *mut u8, total) };
    zlib_rs::gz::gzfread(out, size, nitems, &mut shim.inner)
}

/// C `gzwrite` — write `len` uncompressed bytes from `buf`. Returns the number
/// of bytes written, or 0 on error.
#[cfg(feature = "gz-io")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzwrite(file: gzFile, buf: *const c_void, len: c_uint) -> c_int {
    // SAFETY: `file` is a handle from `new_gzfile_shim` (or null, guarded).
    let shim = match unsafe { translate::gz_state_mut::<GzFileShim>(file) } {
        Some(s) => s,
        None => return 0,
    };
    // SAFETY: `(buf, len)` describe the caller's readable input buffer.
    let input = unsafe { input_slice(buf as *const u8, len) };
    zlib_rs::gz::gzwrite(&mut shim.inner, input)
}

/// C `gzfwrite` — write `nitems` items of `size` bytes each from `buf`. Returns
/// the number of full items written. Guards against `size * nitems` overflow.
#[cfg(feature = "gz-io")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzfwrite(
    buf: *const c_void,
    size: size_t,
    nitems: size_t,
    file: gzFile,
) -> size_t {
    // SAFETY: `file` is a handle from `new_gzfile_shim` (or null, guarded).
    let shim = match unsafe { translate::gz_state_mut::<GzFileShim>(file) } {
        Some(s) => s,
        None => return 0,
    };
    let total = match size.checked_mul(nitems) {
        Some(t) => t,
        None => return 0,
    };
    // SAFETY: `(buf, total)` describe the caller's readable input buffer.
    let input = unsafe { translate::input_slice_sz(buf as *const u8, total) };
    zlib_rs::gz::gzfwrite(input, size, nitems, &mut shim.inner)
}

/// C `gzputs` — write the NUL-terminated string `s` (without its NUL). Returns
/// the number of bytes written, or -1 on error.
#[cfg(feature = "gz-io")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzputs(file: gzFile, s: *const c_char) -> c_int {
    if s.is_null() {
        return -1;
    }
    // SAFETY: `file` is a handle from `new_gzfile_shim` (or null, guarded).
    let shim = match unsafe { translate::gz_state_mut::<GzFileShim>(file) } {
        Some(s) => s,
        None => return -1,
    };
    // SAFETY: `s` is a caller-provided NUL-terminated C string.
    let bytes = unsafe { CStr::from_ptr(s) }.to_bytes();
    zlib_rs::gz::gzputs(&mut shim.inner, bytes)
}

/// C `gzputc` — write one byte `c`. Returns the byte written, or -1 on error.
#[cfg(feature = "gz-io")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzputc(file: gzFile, c: c_int) -> c_int {
    // SAFETY: `file` is a handle from `new_gzfile_shim` (or null, guarded).
    let shim = match unsafe { translate::gz_state_mut::<GzFileShim>(file) } {
        Some(s) => s,
        None => return -1,
    };
    zlib_rs::gz::gzputc(&mut shim.inner, c)
}

/// C `gzgets` — read a line (up to `len - 1` bytes, plus a NUL) into `buf`.
/// Returns `buf` on success, or NULL at EOF/on error.
#[cfg(feature = "gz-io")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzgets(file: gzFile, buf: *mut c_char, len: c_int) -> *mut c_char {
    if buf.is_null() || len <= 0 {
        return core::ptr::null_mut();
    }
    // SAFETY: `file` is a handle from `new_gzfile_shim` (or null, guarded).
    let shim = match unsafe { translate::gz_state_mut::<GzFileShim>(file) } {
        Some(s) => s,
        None => return core::ptr::null_mut(),
    };
    // SAFETY: `(buf, len)` describe the caller's writable line buffer; the core
    // writes at most `len` bytes (line data + NUL terminator).
    let out = unsafe { translate::output_slice(buf as *mut u8, len as c_uint) };
    match zlib_rs::gz::gzgets(&mut shim.inner, out) {
        Some(_) => buf,
        None => core::ptr::null_mut(),
    }
}

/// C `gzgetc` — read one byte. Returns the byte (0..=255) or -1 at EOF/error.
/// This is the function the generated `gzgetc` macro falls back to (we keep the
/// inline `have` counter at 0 so the macro always calls here).
#[cfg(feature = "gz-io")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzgetc(file: gzFile) -> c_int {
    // SAFETY: `file` is a handle from `new_gzfile_shim` (or null, guarded).
    let shim = match unsafe { translate::gz_state_mut::<GzFileShim>(file) } {
        Some(s) => s,
        None => return -1,
    };
    zlib_rs::gz::gzgetc(&mut shim.inner)
}

/// C `gzungetc` — push byte `c` back so the next read returns it. Returns `c`,
/// or -1 on error.
#[cfg(feature = "gz-io")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzungetc(c: c_int, file: gzFile) -> c_int {
    // SAFETY: `file` is a handle from `new_gzfile_shim` (or null, guarded).
    let shim = match unsafe { translate::gz_state_mut::<GzFileShim>(file) } {
        Some(s) => s,
        None => return -1,
    };
    zlib_rs::gz::gzungetc(c, &mut shim.inner)
}

/// C `gzflush` — flush pending output with the given `flush` mode.
#[cfg(feature = "gz-io")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzflush(file: gzFile, flush: c_int) -> c_int {
    // SAFETY: `file` is a handle from `new_gzfile_shim` (or null, guarded).
    let shim = match unsafe { translate::gz_state_mut::<GzFileShim>(file) } {
        Some(s) => s,
        None => return Z_STREAM_ERROR,
    };
    zlib_rs::gz::gzflush(&mut shim.inner, flush)
}

/// C `gzseek` — reposition `file` to `offset` per `whence` (`SEEK_SET`/
/// `SEEK_CUR`). Returns the resulting uncompressed offset, or -1 on error.
#[cfg(feature = "gz-io")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzseek(file: gzFile, offset: off_t, whence: c_int) -> off_t {
    // SAFETY: `file` is a handle from `new_gzfile_shim` (or null, guarded).
    let shim = match unsafe { translate::gz_state_mut::<GzFileShim>(file) } {
        Some(s) => s,
        None => return -1,
    };
    let result = zlib_rs::gz::gzseek(&mut shim.inner, translate::off_t_to_i64(offset), whence);
    translate::i64_to_off_t(result)
}

/// C `gzrewind` — reset `file` to the beginning (read mode only).
#[cfg(feature = "gz-io")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzrewind(file: gzFile) -> c_int {
    // SAFETY: `file` is a handle from `new_gzfile_shim` (or null, guarded).
    let shim = match unsafe { translate::gz_state_mut::<GzFileShim>(file) } {
        Some(s) => s,
        None => return -1,
    };
    zlib_rs::gz::gzrewind(&mut shim.inner)
}

/// C `gztell` — return the current uncompressed offset, or -1 on error.
#[cfg(feature = "gz-io")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gztell(file: gzFile) -> off_t {
    // SAFETY: `file` is a handle from `new_gzfile_shim` (or null, guarded).
    let shim = match unsafe { translate::gz_state_mut::<GzFileShim>(file) } {
        Some(s) => s,
        None => return -1,
    };
    translate::i64_to_off_t(zlib_rs::gz::gztell(&shim.inner))
}

/// C `gzoffset` — return the current compressed-file offset, or -1 on error.
#[cfg(feature = "gz-io")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzoffset(file: gzFile) -> off_t {
    // SAFETY: `file` is a handle from `new_gzfile_shim` (or null, guarded).
    let shim = match unsafe { translate::gz_state_mut::<GzFileShim>(file) } {
        Some(s) => s,
        None => return -1,
    };
    translate::i64_to_off_t(zlib_rs::gz::gzoffset(&mut shim.inner))
}

/// C `gzeof` — return non-zero once the read end-of-file has been reached.
#[cfg(feature = "gz-io")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzeof(file: gzFile) -> c_int {
    // SAFETY: `file` is a handle from `new_gzfile_shim` (or null, guarded).
    let shim = match unsafe { translate::gz_state_mut::<GzFileShim>(file) } {
        Some(s) => s,
        None => return 0,
    };
    zlib_rs::gz::gzeof(&shim.inner)
}

/// C `gzdirect` — return non-zero if `file` is being read transparently (i.e.
/// the input is not actually gzip-compressed).
#[cfg(feature = "gz-io")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzdirect(file: gzFile) -> c_int {
    // SAFETY: `file` is a handle from `new_gzfile_shim` (or null, guarded).
    let shim = match unsafe { translate::gz_state_mut::<GzFileShim>(file) } {
        Some(s) => s,
        None => return 0,
    };
    zlib_rs::gz::gzdirect(&mut shim.inner)
}

/// C `gzclose` — flush (write mode), close, and free `file`. Consumes the
/// handle (reconstructs the owning box and drops it = RAII close).
#[cfg(feature = "gz-io")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzclose(file: gzFile) -> c_int {
    if file.is_null() {
        return Z_STREAM_ERROR;
    }
    // SAFETY: `file` came from `new_gzfile_shim` (`Box::into_raw`); reconstruct
    // the owning box exactly once, then hand the inner handle to the core.
    let shim = *unsafe { Box::from_raw(file as *mut GzFileShim) };
    zlib_rs::gz::gzclose(shim.inner)
}

/// C `gzclose_r` — the read-mode specialization of `gzclose`.
#[cfg(feature = "gz-io")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzclose_r(file: gzFile) -> c_int {
    if file.is_null() {
        return Z_STREAM_ERROR;
    }
    // SAFETY: `file` came from `new_gzfile_shim`; reconstruct the box once.
    let shim = *unsafe { Box::from_raw(file as *mut GzFileShim) };
    zlib_rs::gz::gzclose_r(shim.inner)
}

/// C `gzclose_w` — the write-mode specialization of `gzclose` (flushes output).
#[cfg(feature = "gz-io")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzclose_w(file: gzFile) -> c_int {
    if file.is_null() {
        return Z_STREAM_ERROR;
    }
    // SAFETY: `file` came from `new_gzfile_shim`; reconstruct the box once.
    let shim = *unsafe { Box::from_raw(file as *mut GzFileShim) };
    zlib_rs::gz::gzclose_w(shim.inner)
}

/// C `gzerror` — return the last error message for `file` and (if `errnum` is
/// non-null) write the numeric code through it. The returned pointer stays
/// valid until the next gz call on `file` (it is owned by the shim's `err_buf`).
#[cfg(feature = "gz-io")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzerror(file: gzFile, errnum: *mut c_int) -> *const c_char {
    // SAFETY: `file` is a handle from `new_gzfile_shim` (or null, guarded).
    let shim = match unsafe { translate::gz_state_mut::<GzFileShim>(file) } {
        Some(s) => s,
        None => return core::ptr::null(),
    };
    // Scope the immutable borrow of `shim.inner` so its `&str` result is copied
    // into an owned buffer before we mutate the disjoint `shim.err_buf` field.
    let (num, msg_bytes): (c_int, Option<Vec<u8>>) = {
        let mut n = 0i32;
        let owned = zlib_rs::gz::gzerror(&shim.inner, &mut n).map(|s| s.as_bytes().to_vec());
        (n, owned)
    };
    if !errnum.is_null() {
        // SAFETY: `errnum` is a valid writable `c_int` pointer (guarded).
        unsafe { *errnum = num };
    }
    // Rebuild the NUL-terminated mirror and return a stable pointer into it.
    shim.err_buf.clear();
    if let Some(b) = &msg_bytes {
        shim.err_buf.extend_from_slice(b);
    }
    shim.err_buf.push(0);
    shim.err_buf.as_ptr() as *const c_char
}

/// C `gzclearerr` — clear the error and end-of-file flags for `file`. (void.)
#[cfg(feature = "gz-io")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzclearerr(file: gzFile) {
    // SAFETY: `file` is a handle from `new_gzfile_shim` (or null, guarded).
    if let Some(shim) = unsafe { translate::gz_state_mut::<GzFileShim>(file) } {
        zlib_rs::gz::gzclearerr(&mut shim.inner);
    }
}

// ===========================================================================
// AAP-Phase 7 — gzprintf (the single C-variadic symbol)
//
// `int gzprintf(gzFile, const char *format, ...)` is genuinely C-variadic.
// Rust can *call* C-variadic functions on stable, but *defining* one requires
// the unstable `#![feature(c_variadic)]` (nightly only). It is therefore gated
// behind the `c-variadic` feature.
//
// For a true `zlib.h` DROP-IN (AAP G4 — all 73 symbols, INCLUDING `gzprintf`),
// the `c-variadic` feature MUST be enabled. The `libz-rs-sys-cdylib` drop-in
// member enables it BY DEFAULT, so the shipped `libz.so` / `libz.a` export the
// full 73-symbol surface; the workspace pins a nightly toolchain
// (`rust-toolchain.toml`) so that build succeeds. This thin `libz-rs-sys` crate
// keeps the feature OFF by default so a consumer who does not need the C
// `gzprintf` symbol (or cannot use nightly) still gets the other 72 symbols on
// stable.
//
// We deliberately do NOT fake a non-variadic `gzprintf` with a different
// signature — that would break ABI/header parity. Enabling `c-variadic` on a
// stable toolchain fails to compile (the `feature(c_variadic)` attribute is
// rejected by stable rustc); that compile error is the intended guard.
//
// NOTE (build matrix):
//   * `libz-rs-sys` default (stable):                  72 symbols, ABSENT.
//   * `libz-rs-sys --features c-variadic` (nightly):   73 symbols, PRESENT.
//   * `libz-rs-sys-cdylib` default (nightly):          73 symbols, PRESENT
//                                                       (the shipped drop-in).

/// C `gzprintf` — `printf`-style formatted write to a gz file.
///
/// Renders `format` + the variadic arguments into the gz stream's CURRENT
/// state-sized scratch buffer (honoring any caller `gzbuffer()` resize) and
/// writes the result, routing through the shared
/// [`zlib_rs::gz::gz_printf_into`] so the exact C vprintf-style overflow and
/// accounting discipline is reused rather than re-implemented. Per C zlib,
/// output that is empty, `size`-or-larger, or whose trailing NUL sentinel was
/// overwritten is REJECTED: nothing is written and `0` is returned (it is never
/// silently truncated). Returns the number of *uncompressed* bytes written, or
/// a negative zlib error code.
///
/// Available only under the nightly-only `c-variadic` feature (see the module
/// note above); the `libz-rs-sys-cdylib` drop-in enables it by default.
#[cfg(all(feature = "gz-io", feature = "c-variadic"))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gzprintf(file: gzFile, format: *const c_char, args: ...) -> c_int {
    // The OS `vsnprintf` bound to accept a `core::ffi::VaList`. The `libc`
    // crate's `va_list` is a distinct opaque type, so the C symbol is declared
    // directly here to match the variadic `args` we forward.
    unsafe extern "C" {
        fn vsnprintf(s: *mut c_char, n: size_t, fmt: *const c_char, ap: core::ffi::VaList)
        -> c_int;
    }
    if format.is_null() {
        return Z_STREAM_ERROR;
    }
    // SAFETY: `file` is a handle from `new_gzfile_shim` (or null, guarded).
    let shim = match unsafe { translate::gz_state_mut::<GzFileShim>(file) } {
        Some(s) => s,
        None => return Z_STREAM_ERROR,
    };
    // Route through the shared core discipline: it hands the closure the
    // stream's state-sized scratch `region` (length == the current `gzbuffer()`
    // size) and applies the C `gzvprintf` book-ending (`gz_vacate`, byte
    // accounting) plus the `len == 0 || len >= size` rejection. The closure
    // performs only the `vsnprintf` render and reproduces C zlib's NUL-sentinel
    // overflow check exactly (gzwrite.c L452, L470-471).
    zlib_rs::gz::gz_printf_into(&mut shim.inner, |region| {
        let size = region.len();
        // C: `next[state->size - 1] = 0` — pre-set the last byte so an exactly-
        // filling or overrunning render is detectable afterwards.
        region[size - 1] = 0;
        // SAFETY: the caller guarantees `format` is a valid printf-style string
        // whose conversion specifiers match the variadic `args` (the C
        // `gzprintf` contract); `vsnprintf` renders into `region`, writing at
        // most `size` bytes including the trailing NUL.
        let n = unsafe { vsnprintf(region.as_mut_ptr() as *mut c_char, size, format, args) };
        if n < 0 {
            // Encoding error → write nothing (C treats a non-positive len as 0).
            return None;
        }
        let len = n as usize;
        // C (gzwrite.c L470-471): reject empty, `size`-or-larger, or a clobbered
        // NUL sentinel — exactly the overflow guard C `gzvprintf` applies before
        // committing the formatted bytes.
        if len == 0 || len >= size || region[size - 1] != 0 {
            None
        } else {
            Some(len)
        }
    })
}

// ===========================================================================
// AAP-Phase 8 — Symbols DELIBERATELY EXCLUDED (documented, not exported)
//
// The following symbols appear in `zlib.h` and/or `zlib.map` but are
// intentionally NOT exported from this crate. They are outside the AAP §0.4.1
// authoritative 73-symbol set. The full, versioned symbol set (including the
// 64-bit and `_z` aliases below, wired through a `zlib.map`-derived version
// script) belongs to the downstream `libz-rs-sys-cdylib` workspace member, not
// to this thin `extern "C"` surface.
//
//   (A) 64-bit large-file variants — the `*64` aliases. On a 64-bit `off_t`
//       platform these are ABI-identical to their unsuffixed forms, which we
//       DO export; the cdylib's version script maps the `*64` names onto them.
//         gzopen64, gzseek64, gztell64, gzoffset64,
//         adler32_combine64, crc32_combine64, crc32_combine_gen64
//
//   (B) `size_t`-length one-shot aliases — `*_z` forms that mirror the
//       unsuffixed one-shot helpers with a `size_t` length. Not in the 73-set:
//         compressBound_z, deflateBound_z,
//         compress_z, compress2_z, uncompress_z, uncompress2_z
//
//   (C) Undocumented / auxiliary / internal-helper symbols — not part of the
//       stable documented API, hence excluded:
//         gzgetc_              (the macro's fallback is the exported `gzgetc`)
//         gzvprintf            (internal helper; only under `c-variadic`)
//         inflateUndermine, inflateValidate, inflateCodesUsed,
//         inflateResetKeep, deflateResetKeep, inflateSyncPoint,
//         deflateUsed, gzopen_w
//
//   (D) LOCAL / HIDDEN symbols (from `zlib.map`'s `local:` clause) that MUST
//       NEVER carry C linkage. These exist (if at all) only as PRIVATE items
//       inside the safe `zlib_rs` core; nothing here re-exports them, and none
//       are `#[no_mangle]`:
//         deflate_copyright, inflate_copyright,
//         inflate_fast, inflate_table, inflate_fixed,
//         zcalloc, zcfree, z_errmsg,
//         gz_error, gz_intmax,
//         and the `_*` catch-all.
//
// ===========================================================================
// AAP-Phase 9 — cbindgen reachability & symbol-count audit
//
// Every exported function above is `pub` + `#[unsafe(no_mangle)]` +
// `unsafe extern "C"` and carries its EXACT C name (no Rust renaming), so
// cbindgen emits each verbatim and `cbindgen.toml [export.rename]` stays empty.
//
// Authoritative count (AAP §0.4.1):
//     deflate family ............ 14
//     inflate family ............ 16
//     one-shot helpers ..........  5
//     checksums .................  9
//     version / diagnostics .....  3
//     gz file API ............... 26   (gated `#[cfg(feature = "gz-io")]`)
//                                 ----
//     TOTAL ..................... 73
//
// Build matrix:
//   * default (std + gz-io, stable):     72 symbols  (gzprintf OMITTED)
//   * + `c-variadic` (nightly):          73 symbols  (gzprintf PRESENT)
//   * `--no-default-features` (no gz):   47 symbols  (entire gz block excluded)
//
// The 47-symbol core (no gz) = 14 + 16 + 5 + 9 + 3.
// ===========================================================================

// ===========================================================================
// Ad-hoc validation tests — exercise the FFI surface end-to-end.
//
// These prove the thin shim correctly bridges the C ABI to the safe core:
//   * the canonical ABI version string,
//   * checksum known-answer vectors and the NULL-buffer special cases,
//   * the one-shot `compress`/`uncompress` path with `compressBound` sizing,
//   * the full streaming `deflate(Z_FINISH)` -> `inflate` round-trip, and
//   * the `Z_VERSION_ERROR` init guard.
//
// Gated on `std`: these tests use `vec!`/`Vec` and the standard `#[test]`
// harness, neither of which is available under the `no_std` (no-default)
// build, where `--all-targets` would otherwise try to compile this module.
// ===========================================================================
#[cfg(all(test, feature = "std"))]
mod tests {
    use super::*;
    use core::ptr;

    /// `zlibVersion()` must return exactly the canonical ABI string — this is
    /// distinct from the crate's Cargo semver (AAP Phase 5 / Final Validation).
    #[test]
    fn version_string_is_canonical() {
        // SAFETY: `zlibVersion` returns a pointer to a 'static NUL-terminated C
        // string; reading it back through `CStr` is sound.
        let v = unsafe { core::ffi::CStr::from_ptr(zlibVersion()) };
        assert_eq!(v.to_str().unwrap(), "1.3.2.1-motley");
        assert_eq!(ZLIB_VERNUM, 0x1321);
    }

    /// Checksum known-answer vectors plus the documented NULL-buffer rules:
    /// `crc32(_, NULL, _) == 0` and `adler32(_, NULL, _) == 1`.
    #[test]
    fn checksum_known_answers() {
        let data = b"123456789";
        // SAFETY: `data` is a valid 9-byte buffer.
        let crc = unsafe { crc32(0, data.as_ptr(), data.len() as c_uint) };
        assert_eq!(crc, 0xCBF4_3926);

        let w = b"Wikipedia";
        // SAFETY: `w` is a valid 9-byte buffer.
        let a = unsafe { adler32(1, w.as_ptr(), w.len() as c_uint) };
        assert_eq!(a, 0x11E6_0398);

        // NULL-buffer special cases (return the respective identity values).
        // SAFETY: a NULL buffer is the explicitly handled sentinel.
        assert_eq!(unsafe { crc32(0, ptr::null(), 0) }, 0);
        // SAFETY: a NULL buffer is the explicitly handled sentinel.
        assert_eq!(unsafe { adler32(0, ptr::null(), 0) }, 1);
    }

    /// One-shot `compress` -> `uncompress` round-trip; `compressBound` must be
    /// an upper bound on the produced size.
    #[test]
    fn one_shot_compress_uncompress_round_trip() {
        let src = b"The quick brown fox jumps over the lazy dog. ".repeat(20);

        // SAFETY: scalar-only call.
        let bound = unsafe { compressBound(src.len() as c_ulong) };
        let mut comp = vec![0u8; bound as usize];
        let mut comp_len = bound;
        // SAFETY: `comp`/`src` are valid buffers; `comp_len` is a valid out-ptr
        // initialized to the capacity of `comp`.
        let rc = unsafe {
            compress(
                comp.as_mut_ptr(),
                &mut comp_len,
                src.as_ptr(),
                src.len() as c_ulong,
            )
        };
        assert_eq!(rc, Z_OK);
        assert!(comp_len <= bound);
        comp.truncate(comp_len as usize);

        let mut deco = vec![0u8; src.len()];
        let mut deco_len = deco.len() as c_ulong;
        // SAFETY: `deco`/`comp` are valid buffers; `deco_len` is a valid out-ptr
        // initialized to the capacity of `deco`.
        let rc = unsafe {
            uncompress(
                deco.as_mut_ptr(),
                &mut deco_len,
                comp.as_ptr(),
                comp.len() as c_ulong,
            )
        };
        assert_eq!(rc, Z_OK);
        deco.truncate(deco_len as usize);
        assert_eq!(deco, src);
    }

    /// The documented full FFI path:
    /// `deflateInit_` -> `deflate(Z_FINISH)` -> `deflateEnd` ->
    /// `inflateInit_` -> `inflate` -> `inflateEnd` reproduces the input.
    #[test]
    fn streaming_deflate_inflate_round_trip() {
        let src = b"hello hello hello world world world FFI round trip!".repeat(10);
        let stream_size = core::mem::size_of::<z_stream>() as c_int;

        // ---- deflate ----
        let mut comp = vec![0u8; 8192];
        let mut strm = z_stream {
            next_in: src.as_ptr(),
            avail_in: src.len() as c_uint,
            next_out: comp.as_mut_ptr(),
            avail_out: comp.len() as c_uint,
            ..Default::default()
        };
        // SAFETY: `strm` is a valid, fully-initialized z_stream; `zlibVersion()`
        // is the matching ABI version; `stream_size` is `sizeof(z_stream)`.
        let rc =
            unsafe { deflateInit_(&mut strm, Z_DEFAULT_COMPRESSION, zlibVersion(), stream_size) };
        assert_eq!(rc, Z_OK);
        // SAFETY: `strm` was successfully initialized above.
        let rc = unsafe { deflate(&mut strm, Z_FINISH) };
        assert_eq!(rc, Z_STREAM_END);
        let comp_len = strm.total_out as usize;
        // SAFETY: `strm` is an initialized deflate stream.
        assert_eq!(unsafe { deflateEnd(&mut strm) }, Z_OK);
        comp.truncate(comp_len);

        // ---- inflate ----
        let mut deco = vec![0u8; src.len()];
        let mut istrm = z_stream {
            next_in: comp.as_ptr(),
            avail_in: comp.len() as c_uint,
            next_out: deco.as_mut_ptr(),
            avail_out: deco.len() as c_uint,
            ..Default::default()
        };
        // SAFETY: `istrm` is valid; version/size match as above.
        let rc = unsafe { inflateInit_(&mut istrm, zlibVersion(), stream_size) };
        assert_eq!(rc, Z_OK);
        // SAFETY: `istrm` was successfully initialized above.
        let rc = unsafe { inflate(&mut istrm, Z_NO_FLUSH) };
        assert_eq!(rc, Z_STREAM_END);
        let deco_len = istrm.total_out as usize;
        // SAFETY: `istrm` is an initialized inflate stream.
        assert_eq!(unsafe { inflateEnd(&mut istrm) }, Z_OK);
        deco.truncate(deco_len);

        assert_eq!(deco, src);
    }

    /// Regression for the FINAL-checkpoint MAJOR finding: `inflateGetHeader`
    /// must POPULATE the caller's `gz_header` `name` / `comment` / `extra`
    /// buffers, not silently discard the parsed bytes. Drives a full gzip
    /// round-trip through the C ABI — `deflateInit2_(windowBits = 31)` +
    /// `deflateSetHeader` + `deflate(Z_FINISH)`, then `inflateInit2_(31)` +
    /// `inflateGetHeader` + `inflate(Z_FINISH)` — and asserts every header
    /// field is recovered, mirroring the QA reproduction (`gzhdr_full.c`).
    ///
    /// Before the fix, `inflateGetHeader` handed the core an all-`None`
    /// `GzHeader`, so `inflate` consumed and dropped the name/comment/extra
    /// bytes, leaving these caller buffers empty (and `extra_len == 0`).
    #[cfg(feature = "gzip")]
    #[test]
    fn gzip_header_round_trip_recovers_name_comment_extra() {
        let stream_size = core::mem::size_of::<z_stream>() as c_int;
        let payload = b"data-payload for the gzip header capture regression test";

        // ---- deflate a gzip member carrying name/comment/extra + scalars ----
        // `deflateSetHeader` reads `name`/`comment` as NUL-terminated C strings
        // and `extra` for `extra_len` bytes, so the inputs are shaped to match.
        let mut name_in = *b"file.txt\0";
        let mut comment_in = *b"a comment\0";
        let mut extra_in = [1u8, 2, 3, 4, 5];

        let mut comp = vec![0u8; 4096];
        let mut dstrm = z_stream {
            next_in: payload.as_ptr(),
            avail_in: payload.len() as c_uint,
            next_out: comp.as_mut_ptr(),
            avail_out: comp.len() as c_uint,
            ..Default::default()
        };
        // SAFETY: `dstrm` is valid; windowBits 31 selects gzip; version/size match.
        let rc = unsafe {
            deflateInit2_(
                &mut dstrm,
                Z_DEFAULT_COMPRESSION,
                Z_DEFLATED,
                31,
                8,
                Z_DEFAULT_STRATEGY,
                zlibVersion(),
                stream_size,
            )
        };
        assert_eq!(rc, Z_OK);

        let mut dhead = gz_header {
            text: 1,
            time: 0x1122_3344,
            os: 3,
            extra: extra_in.as_mut_ptr(),
            extra_len: extra_in.len() as c_uint,
            name: name_in.as_mut_ptr(),
            comment: comment_in.as_mut_ptr(),
            ..Default::default()
        };
        // SAFETY: `dstrm` is an initialized gzip deflate stream; `dhead`'s
        // buffers are valid for the lengths advertised above.
        assert_eq!(unsafe { deflateSetHeader(&mut dstrm, &mut dhead) }, Z_OK);
        // SAFETY: initialized deflate stream with valid in/out buffers.
        assert_eq!(unsafe { deflate(&mut dstrm, Z_FINISH) }, Z_STREAM_END);
        let comp_len = dstrm.total_out as usize;
        // SAFETY: initialized deflate stream.
        assert_eq!(unsafe { deflateEnd(&mut dstrm) }, Z_OK);
        comp.truncate(comp_len);

        // ---- inflate and capture the header back into the caller buffers ----
        let mut name_out = [0u8; 64];
        let mut comment_out = [0u8; 64];
        let mut extra_out = [0u8; 64];
        let mut ihead = gz_header {
            name: name_out.as_mut_ptr(),
            name_max: name_out.len() as c_uint,
            comment: comment_out.as_mut_ptr(),
            comm_max: comment_out.len() as c_uint,
            extra: extra_out.as_mut_ptr(),
            extra_max: extra_out.len() as c_uint,
            ..Default::default()
        };

        let mut deco = vec![0u8; payload.len() + 64];
        let mut istrm = z_stream {
            next_in: comp.as_ptr(),
            avail_in: comp.len() as c_uint,
            next_out: deco.as_mut_ptr(),
            avail_out: deco.len() as c_uint,
            ..Default::default()
        };
        // SAFETY: `istrm` is valid; windowBits 31 selects gzip; version/size match.
        assert_eq!(
            unsafe { inflateInit2_(&mut istrm, 31, zlibVersion(), stream_size) },
            Z_OK
        );
        // SAFETY: `istrm` is initialized; `ihead`'s buffers are valid for the
        // `*_max` capacities advertised above. This is the call the fix repairs.
        assert_eq!(unsafe { inflateGetHeader(&mut istrm, &mut ihead) }, Z_OK);
        // SAFETY: initialized inflate stream with valid buffers.
        assert_eq!(unsafe { inflate(&mut istrm, Z_FINISH) }, Z_STREAM_END);
        let deco_len = istrm.total_out as usize;
        // SAFETY: initialized inflate stream.
        assert_eq!(unsafe { inflateEnd(&mut istrm) }, Z_OK);

        // The header must be fully parsed and every field recovered.
        assert_eq!(ihead.done, 1, "header parsing should be marked done");
        assert_eq!(ihead.text, 1, "FTEXT flag should round-trip");
        assert_eq!(ihead.time, 0x1122_3344, "MTIME should round-trip");
        assert_eq!(ihead.os, 3, "OS code should round-trip");

        // name / comment are written back NUL-terminated (the regression target).
        // SAFETY: `ihead.name` points to our valid, NUL-terminated 64-byte buffer.
        let got_name = unsafe { core::ffi::CStr::from_ptr(ihead.name as *const c_char) };
        assert_eq!(got_name.to_bytes(), b"file.txt", "name must be recovered");
        // SAFETY: `ihead.comment` points to our valid, NUL-terminated buffer.
        let got_comment = unsafe { core::ffi::CStr::from_ptr(ihead.comment as *const c_char) };
        assert_eq!(
            got_comment.to_bytes(),
            b"a comment",
            "comment must be recovered"
        );

        // extra reports its true on-wire length and exact bytes.
        assert_eq!(ihead.extra_len, 5, "extra_len must be the on-wire XLEN");
        assert_eq!(
            &extra_out[..5],
            &[1u8, 2, 3, 4, 5],
            "extra bytes must be recovered"
        );

        // The compressed payload itself must still round-trip intact.
        assert_eq!(&deco[..deco_len], &payload[..]);
    }

    /// A mismatched version string must yield `Z_VERSION_ERROR` (the init guard).
    #[test]
    fn version_error_on_bad_version() {
        let mut strm = z_stream::default();
        let bad = c"0.0.0";
        // SAFETY: `strm` is valid; `bad` is a NUL-terminated C string; the size
        // is correct — only the version mismatch should be detected.
        let rc = unsafe {
            deflateInit_(
                &mut strm,
                Z_DEFAULT_COMPRESSION,
                bad.as_ptr(),
                core::mem::size_of::<z_stream>() as c_int,
            )
        };
        assert_eq!(rc, Z_VERSION_ERROR);
    }

    /// Helper: one-shot FFI deflate of `src` at the default level, returning the
    /// compressed bytes. Used by the streaming-totals tests below.
    fn ffi_deflate(src: &[u8]) -> Vec<u8> {
        let stream_size = core::mem::size_of::<z_stream>() as c_int;
        let mut comp = vec![0u8; src.len() + 256];
        let mut strm = z_stream {
            next_in: src.as_ptr(),
            avail_in: src.len() as c_uint,
            next_out: comp.as_mut_ptr(),
            avail_out: comp.len() as c_uint,
            ..Default::default()
        };
        // SAFETY: `strm` is fully initialized; version/size match the ABI.
        unsafe {
            assert_eq!(
                deflateInit_(&mut strm, Z_DEFAULT_COMPRESSION, zlibVersion(), stream_size),
                Z_OK
            );
            assert_eq!(deflate(&mut strm, Z_FINISH), Z_STREAM_END);
        }
        let comp_len = strm.total_out as usize;
        // SAFETY: `strm` is an initialized deflate stream.
        assert_eq!(unsafe { deflateEnd(&mut strm) }, Z_OK);
        comp.truncate(comp_len);
        comp
    }

    /// Regression for the CP3 FFI `inflate()` streaming-totals finding: driving
    /// `inflate()` across MANY productive calls with a tiny **output** window must
    /// leave `total_in == compressed_len` and `total_out == original_len` — i.e.
    /// the cumulative counters must NOT double-count per-call deltas. The smoke
    /// test elsewhere only checks bytes (and uses `truncate`, which cannot grow),
    /// so it could not catch a doubled `total_out`; these assertions can.
    #[test]
    fn ffi_inflate_streaming_totals_small_output() {
        let src = b"streaming totals: small output window stresses many calls. ".repeat(64);
        let comp = ffi_deflate(&src);
        let stream_size = core::mem::size_of::<z_stream>() as c_int;

        let mut deco = vec![0u8; src.len() + 64];
        let mut istrm = z_stream {
            next_in: comp.as_ptr(),
            avail_in: comp.len() as c_uint,
            next_out: deco.as_mut_ptr(),
            avail_out: 0,
            ..Default::default()
        };
        // SAFETY: `istrm` is valid; version/size match the ABI.
        assert_eq!(
            unsafe { inflateInit_(&mut istrm, zlibVersion(), stream_size) },
            Z_OK
        );

        let mut produced = 0usize;
        let mut calls = 0usize;
        loop {
            // Hand the engine a fresh 13-byte output window each call.
            // SAFETY: `produced <= deco.len()`, so the offset is in-bounds.
            istrm.next_out = unsafe { deco.as_mut_ptr().add(produced) };
            istrm.avail_out = core::cmp::min(13, deco.len() - produced) as c_uint;
            let before = istrm.avail_out;
            // SAFETY: `istrm` is an initialized inflate stream with valid buffers.
            let rc = unsafe { inflate(&mut istrm, Z_NO_FLUSH) };
            produced += (before - istrm.avail_out) as usize;
            calls += 1;
            if rc == Z_STREAM_END {
                break;
            }
            assert_eq!(rc, Z_OK, "unexpected inflate rc {rc} at call {calls}");
            assert!(calls < 100_000, "inflate loop failed to terminate");
        }

        // The decisive assertions: cumulative totals must be exact, not doubled.
        assert_eq!(
            istrm.total_in as usize,
            comp.len(),
            "total_in must equal compressed length across multi-call inflate"
        );
        assert_eq!(
            istrm.total_out as usize,
            src.len(),
            "total_out must equal original length (no double-count) across multi-call inflate"
        );
        // SAFETY: `istrm` is an initialized inflate stream.
        assert_eq!(unsafe { inflateEnd(&mut istrm) }, Z_OK);
        assert_eq!(&deco[..produced], &src[..]);
        assert!(calls > 1, "test must exercise multiple productive calls");
    }

    /// Companion to the above: stream the **input** in tiny `avail_in` chunks
    /// (ample output) and assert the cumulative totals are still exact. This
    /// exercises the other productive-call shape (many partial-input calls).
    #[test]
    fn ffi_inflate_streaming_totals_small_input() {
        let src = b"small-input chunks across the FFI inflate boundary. ".repeat(64);
        let comp = ffi_deflate(&src);
        let stream_size = core::mem::size_of::<z_stream>() as c_int;

        let mut deco = vec![0u8; src.len() + 64];
        let mut istrm = z_stream {
            next_in: comp.as_ptr(),
            avail_in: 0,
            next_out: deco.as_mut_ptr(),
            avail_out: deco.len() as c_uint,
            ..Default::default()
        };
        // SAFETY: `istrm` is valid; version/size match the ABI.
        assert_eq!(
            unsafe { inflateInit_(&mut istrm, zlibVersion(), stream_size) },
            Z_OK
        );

        let mut fed = 0usize;
        let mut calls = 0usize;
        loop {
            // Feed at most 7 more input bytes each call.
            if fed < comp.len() {
                let grant = core::cmp::min(7, comp.len() - fed);
                // SAFETY: `fed <= comp.len()`, so the offset is in-bounds.
                istrm.next_in = unsafe { comp.as_ptr().add(fed) };
                istrm.avail_in = grant as c_uint;
                fed += grant;
            }
            // SAFETY: `istrm` is an initialized inflate stream with valid buffers.
            let rc = unsafe { inflate(&mut istrm, Z_NO_FLUSH) };
            calls += 1;
            if rc == Z_STREAM_END {
                break;
            }
            assert!(
                rc == Z_OK || rc == Z_BUF_ERROR,
                "unexpected inflate rc {rc} at call {calls}"
            );
            assert!(calls < 100_000, "inflate loop failed to terminate");
        }

        assert_eq!(
            istrm.total_in as usize,
            comp.len(),
            "total_in must equal compressed length across small-input inflate"
        );
        assert_eq!(
            istrm.total_out as usize,
            src.len(),
            "total_out must equal original length across small-input inflate"
        );
        // SAFETY: `istrm` is an initialized inflate stream.
        assert_eq!(unsafe { inflateEnd(&mut istrm) }, Z_OK);
        assert_eq!(&deco[..src.len()], &src[..]);
        assert!(calls > 1, "test must exercise multiple productive calls");
    }

    /// Deflate-path regression guard: the deflate write-back still accumulates
    /// per-call deltas, so multi-call deflate with a tiny **output** window must
    /// still report `total_in == original_len` and `total_out == compressed_len`.
    /// (Confirms the inflate-specific write-back split did not perturb deflate.)
    #[test]
    fn ffi_deflate_streaming_totals_small_output() {
        let src = b"deflate streaming totals across small output windows. ".repeat(64);
        let stream_size = core::mem::size_of::<z_stream>() as c_int;

        let mut comp = vec![0u8; src.len() + 256];
        let mut strm = z_stream {
            next_in: src.as_ptr(),
            avail_in: src.len() as c_uint,
            next_out: comp.as_mut_ptr(),
            avail_out: 0,
            ..Default::default()
        };
        // SAFETY: `strm` is valid; version/size match the ABI.
        assert_eq!(
            unsafe { deflateInit_(&mut strm, Z_DEFAULT_COMPRESSION, zlibVersion(), stream_size) },
            Z_OK
        );

        let mut produced = 0usize;
        let mut calls = 0usize;
        loop {
            // SAFETY: `produced <= comp.len()`, so the offset is in-bounds.
            strm.next_out = unsafe { comp.as_mut_ptr().add(produced) };
            strm.avail_out = core::cmp::min(11, comp.len() - produced) as c_uint;
            let before = strm.avail_out;
            // SAFETY: `strm` is an initialized deflate stream with valid buffers.
            let rc = unsafe { deflate(&mut strm, Z_FINISH) };
            produced += (before - strm.avail_out) as usize;
            calls += 1;
            if rc == Z_STREAM_END {
                break;
            }
            assert!(
                rc == Z_OK || rc == Z_BUF_ERROR,
                "unexpected deflate rc {rc} at call {calls}"
            );
            assert!(calls < 100_000, "deflate loop failed to terminate");
        }

        assert_eq!(
            strm.total_in as usize,
            src.len(),
            "deflate total_in must equal original length across multi-call deflate"
        );
        assert_eq!(
            strm.total_out as usize, produced,
            "deflate total_out must equal the produced compressed length"
        );
        // SAFETY: `strm` is an initialized deflate stream.
        assert_eq!(unsafe { deflateEnd(&mut strm) }, Z_OK);
        assert!(calls > 1, "test must exercise multiple productive calls");
    }

    /// End-to-end exercise of the C-variadic `gzprintf` (the 73rd drop-in
    /// symbol): write a formatted record to a gz file through the FFI, then read
    /// it back through the FFI and confirm the bytes round-trip. Proves the
    /// symbol is not merely present but functional.
    #[cfg(feature = "c-variadic")]
    #[test]
    fn ffi_gzprintf_round_trip() {
        use std::ffi::CString;

        let mut p = std::env::temp_dir();
        p.push(format!(
            "libz_rs_sys_ffi_gzprintf_rt_{}.gz",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&p);
        let path = CString::new(p.to_string_lossy().as_bytes()).unwrap();
        let expected = "value=42 str=hi end";

        // SAFETY: `path` / mode are valid NUL-terminated C strings.
        let f = unsafe { gzopen(path.as_ptr(), c"wb".as_ptr()) };
        assert!(!f.is_null(), "gzopen(wb) failed");
        // SAFETY: `f` is an open write handle; the format's `%d` / `%s` match the
        // `c_int` / C-string variadic arguments.
        let n = unsafe {
            gzprintf(
                f,
                c"value=%d str=%s end".as_ptr(),
                42 as c_int,
                c"hi".as_ptr(),
            )
        };
        assert_eq!(n, expected.len() as c_int, "gzprintf returned wrong length");
        // SAFETY: `f` is an open handle.
        assert_eq!(unsafe { gzclose(f) }, Z_OK);

        // SAFETY: `path` / mode are valid C strings.
        let g = unsafe { gzopen(path.as_ptr(), c"rb".as_ptr()) };
        assert!(!g.is_null(), "gzopen(rb) failed");
        let mut buf = [0u8; 128];
        // SAFETY: `g` is open for read; `buf` is valid for `buf.len()` bytes.
        let got = unsafe { gzread(g, buf.as_mut_ptr() as *mut c_void, buf.len() as c_uint) };
        assert_eq!(got, expected.len() as c_int, "gzread returned wrong length");
        assert_eq!(&buf[..got as usize], expected.as_bytes());
        // SAFETY: `g` is an open handle.
        assert_eq!(unsafe { gzclose(g) }, Z_OK);

        let _ = std::fs::remove_file(&p);
    }

    /// The CP3 A2 regression: the C-variadic `gzprintf` must reproduce C zlib's
    /// overflow rule — output that does not fit the stream's CURRENT
    /// `gzbuffer()`-sized scratch is REJECTED (`return 0`, nothing written),
    /// never silently truncated, and the buffer size honored is the caller's,
    /// not a fixed `GZBUFSIZE`.
    #[cfg(feature = "c-variadic")]
    #[test]
    fn ffi_gzprintf_rejects_overflow_honoring_gzbuffer() {
        use std::ffi::CString;

        let mut p = std::env::temp_dir();
        p.push(format!(
            "libz_rs_sys_ffi_gzprintf_ovf_{}.gz",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&p);
        let path = CString::new(p.to_string_lossy().as_bytes()).unwrap();

        // SAFETY: valid path / mode C strings.
        let f = unsafe { gzopen(path.as_ptr(), c"wb".as_ptr()) };
        assert!(!f.is_null(), "gzopen(wb) failed");
        // Shrink the buffer to its minimum (8) BEFORE the first write; a fresh
        // `GZBUFSIZE` buffer (the old bug) would be 8 KiB and accept the string.
        // SAFETY: `f` is open and its buffers are not yet allocated.
        assert_eq!(unsafe { gzbuffer(f, 8) }, 0);

        // A formatted output >= 8 bytes must be rejected with 0 (no truncation).
        // SAFETY: `f` open; `%s` matches the C-string argument.
        let n = unsafe { gzprintf(f, c"%s".as_ptr(), c"definitely longer than eight".as_ptr()) };
        assert_eq!(n, 0, "oversize gzprintf must return 0, never a truncation");

        // A 1-byte output still fits in the 8-byte buffer and succeeds, proving
        // the stream remains usable after the rejection.
        // SAFETY: `f` open; `%d` matches the `c_int` argument.
        let n2 = unsafe { gzprintf(f, c"%d".as_ptr(), 7 as c_int) };
        assert_eq!(n2, 1, "1-byte output must fit the 8-byte buffer");

        // SAFETY: `f` is an open handle.
        assert_eq!(unsafe { gzclose(f) }, Z_OK);
        let _ = std::fs::remove_file(&p);
    }

    // -----------------------------------------------------------------------
    // Custom allocator (`zalloc`/`zfree`/`opaque`) — end-to-end FFI behavior.
    //
    // These tests exercise the C allocation-callback path that the safe core
    // alone cannot reach. A `MemZone` cookie (the analogue of `infcover.c`'s
    // `mem_zone`) records outstanding bytes and enforces an optional limit, so
    // we can prove (a) the caller's `zalloc`/`zfree` are genuinely invoked and
    // balanced, and (b) a `zalloc` failure (NULL) surfaces as `Z_MEM_ERROR` —
    // the very behaviors the ported `infcover` mem-zone cases assert.
    // -----------------------------------------------------------------------

    /// A memory-accounting cookie reached through `z_stream.opaque`, mirroring
    /// `infcover.c`'s `mem_zone`: it tracks live bytes, enforces an optional
    /// byte `limit` (`0` = unlimited), and counts successful allocations.
    struct MemZone {
        limit: usize,
        total: usize,
        allocs: usize,
        blocks: Vec<(*mut c_void, usize)>,
    }

    impl MemZone {
        fn new(limit: usize) -> Self {
            Self {
                limit,
                total: 0,
                allocs: 0,
                blocks: Vec::new(),
            }
        }
    }

    /// `alloc_func` honoring the zone's limit, backed by `libc::malloc` so the
    /// returned block is genuinely caller-owned (freed via `mem_free`).
    unsafe extern "C" fn mem_alloc(
        opaque: *mut c_void,
        items: c_uint,
        size: c_uint,
    ) -> *mut c_void {
        // SAFETY: `opaque` is the `&mut MemZone` we install on the stream.
        let zone = unsafe { &mut *(opaque as *mut MemZone) };
        let len = items as usize * size as usize;
        if zone.limit != 0 && zone.total + len > zone.limit {
            return ptr::null_mut();
        }
        // SAFETY: a positive size is requested from the C allocator.
        let buf = unsafe { libc::malloc(len.max(1)) };
        if buf.is_null() {
            return buf;
        }
        zone.total += len;
        zone.allocs += 1;
        zone.blocks.push((buf, len));
        buf
    }

    /// `free_func` matching [`mem_alloc`]: decrements the live-byte count and
    /// returns the block to `libc::free`.
    unsafe extern "C" fn mem_free(opaque: *mut c_void, address: *mut c_void) {
        // SAFETY: `opaque` is the `&mut MemZone` we install on the stream.
        let zone = unsafe { &mut *(opaque as *mut MemZone) };
        if let Some(pos) = zone.blocks.iter().position(|&(p, _)| p == address) {
            let (_, len) = zone.blocks.remove(pos);
            zone.total -= len;
        }
        // SAFETY: `address` was returned by `mem_alloc`'s `libc::malloc`.
        unsafe { libc::free(address) };
    }

    /// The caller's `zalloc`/`zfree` are genuinely invoked by `deflateInit2_`
    /// and fully balanced by `deflateEnd` (no leak) — proving the custom
    /// allocation extension point is wired end-to-end, not merely stored.
    #[test]
    fn ffi_custom_allocator_is_invoked_and_balanced() {
        let mut zone = MemZone::new(0); // unlimited
        let src = b"the quick brown fox jumps over the lazy dog".repeat(8);
        let mut comp = vec![0u8; 8192];
        let stream_size = core::mem::size_of::<z_stream>() as c_int;

        let mut strm = z_stream {
            next_in: src.as_ptr(),
            avail_in: src.len() as c_uint,
            next_out: comp.as_mut_ptr(),
            avail_out: comp.len() as c_uint,
            zalloc: Some(mem_alloc),
            zfree: Some(mem_free),
            opaque: (&mut zone as *mut MemZone).cast(),
            ..Default::default()
        };

        // SAFETY: `strm` is fully initialized with valid custom callbacks.
        let rc = unsafe {
            deflateInit2_(
                &mut strm,
                Z_DEFAULT_COMPRESSION,
                Z_DEFLATED,
                15,
                8,
                Z_DEFAULT_STRATEGY,
                zlibVersion(),
                stream_size,
            )
        };
        assert_eq!(rc, Z_OK);
        assert!(zone.allocs > 0, "custom zalloc must be invoked at init");
        assert!(zone.total > 0, "buffers must be outstanding while live");

        // SAFETY: initialized deflate stream.
        assert_eq!(unsafe { deflate(&mut strm, Z_FINISH) }, Z_STREAM_END);
        // SAFETY: initialized deflate stream.
        assert_eq!(unsafe { deflateEnd(&mut strm) }, Z_OK);

        // Every `zalloc`'d block was returned through `zfree` at teardown.
        assert_eq!(zone.total, 0, "custom zfree must balance every allocation");
        assert!(zone.blocks.is_empty(), "no block may leak past deflateEnd");
    }

    /// A `zalloc` that fails (returns NULL) makes `deflateInit2_` report
    /// `Z_MEM_ERROR` instead of aborting — the C `Z_MEM_ERROR` init path, now
    /// honored through the FFI allocator adapter.
    #[test]
    fn ffi_deflate_init_failing_allocator_is_mem_error() {
        let mut zone = MemZone::new(1); // any real allocation exceeds 1 byte
        let stream_size = core::mem::size_of::<z_stream>() as c_int;
        let mut strm = z_stream {
            zalloc: Some(mem_alloc),
            zfree: Some(mem_free),
            opaque: (&mut zone as *mut MemZone).cast(),
            ..Default::default()
        };
        // SAFETY: `strm` is valid; the failing allocator forces `Z_MEM_ERROR`.
        let rc = unsafe {
            deflateInit2_(
                &mut strm,
                Z_DEFAULT_COMPRESSION,
                Z_DEFLATED,
                15,
                8,
                Z_DEFAULT_STRATEGY,
                zlibVersion(),
                stream_size,
            )
        };
        assert_eq!(
            rc, Z_MEM_ERROR,
            "failing zalloc at init must be Z_MEM_ERROR"
        );
        assert_eq!(
            zone.total, 0,
            "no block should remain outstanding on failure"
        );
    }

    /// The C `inflate` window-allocation failure path: a raw stream whose
    /// decode forces a sliding-window allocation, driven with a `zalloc` that
    /// fails, must return `Z_MEM_ERROR`. This is the FFI realization of the
    /// `infcover` mem-zone case that the safe-core port documents as exercised
    /// here (it cannot fail through the global allocator).
    #[test]
    fn ffi_inflate_window_alloc_failure_is_mem_error() {
        // The `infcover.c` "force window allocation" fixture: a 1-byte raw
        // DEFLATE output that must be saved into the window.
        let input = [0x63u8, 0x00u8];
        let mut out = [0u8; 1];
        let mut zone = MemZone::new(1); // refuse the window allocation
        let stream_size = core::mem::size_of::<z_stream>() as c_int;

        let mut strm = z_stream {
            next_in: input.as_ptr(),
            avail_in: input.len() as c_uint,
            next_out: out.as_mut_ptr(),
            avail_out: out.len() as c_uint,
            zalloc: Some(mem_alloc),
            zfree: Some(mem_free),
            opaque: (&mut zone as *mut MemZone).cast(),
            ..Default::default()
        };

        // Raw inflate (windowBits = -15). Init does not allocate the window
        // (it is lazy), so it succeeds even under the 1-byte limit.
        // SAFETY: `strm` is valid; raw windowBits with a failing allocator.
        let rc = unsafe { inflateInit2_(&mut strm, -15, zlibVersion(), stream_size) };
        assert_eq!(rc, Z_OK, "inflateInit2_ does not allocate the window");

        // The decode produces output that must be windowed; the window
        // allocation is refused, so inflate reports Z_MEM_ERROR.
        // SAFETY: initialized inflate stream.
        let rc = unsafe { inflate(&mut strm, Z_NO_FLUSH) };
        assert_eq!(
            rc, Z_MEM_ERROR,
            "refused window allocation must be Z_MEM_ERROR"
        );

        // SAFETY: initialized inflate stream; teardown frees any live blocks.
        let _ = unsafe { inflateEnd(&mut strm) };
        assert_eq!(
            zone.total, 0,
            "no block should remain outstanding on failure"
        );
    }
}
