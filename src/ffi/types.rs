//! `#[repr(C)]` ABI mirror of zlib's public structs plus the raw ↔ idiomatic
//! conversion glue for the C drop-in boundary.
//!
//! This is the **foundational** module of the [`crate::ffi`] layer. Every other
//! FFI shim (`util.rs`, `deflate.rs`, `inflate.rs`, `gz.rs`) and the module root
//! (`mod.rs`) builds on the types, aliases, allocator bridge, and conversion
//! helpers defined here. The module deliberately contains **no**
//! `#[no_mangle] extern "C"` exported functions — those live in the sibling shim
//! files; here we provide only:
//!
//! * **Scalar type aliases** ([`Bytef`], [`uInt`], [`uLong`], …) mirroring
//!   `zconf.h`, so the shims can spell C types precisely.
//! * **C function-pointer typedefs** ([`alloc_func`], [`free_func`], [`in_func`],
//!   [`out_func`]) modelled as null-pointer-optimized `Option<unsafe extern "C"
//!   fn(…)>`.
//! * **`#[repr(C)]` mirror structs** — [`z_stream`] (`zlib.h` L90-L110),
//!   [`gz_header`] (`zlib.h` L118-L133), and [`gzFile_s`] (`zlib.h` L1956-L1960)
//!   — whose field order and widths match `zlib.h` **exactly** so the emitted
//!   `cdylib`/`staticlib` is byte-layout-compatible with `libz`.
//! * **The caller-allocator bridge** [`CAllocator`], which lets a C caller's
//!   `zalloc`/`zfree`/`opaque` triple ride inside the crate's own
//!   [`Allocator`](crate::stream::Allocator) abstraction.
//! * **Raw ↔ idiomatic conversion helpers** that the deflate/inflate/util shims
//!   use to move between the raw [`z_stream`]/[`gz_header`] and the idiomatic
//!   [`ZStream`](crate::stream::ZStream)/[`GzHeader`](crate::gz_header::GzHeader).
//! * **Panic guards** ([`guard_int`], …) so a shim body can never unwind across
//!   the C boundary.
//!
//! # Unsafe boundary
//!
//! `src/ffi/**` is the crate's designated `unsafe` boundary (AAP §0.6.2 /
//! §0.7.2): the idiomatic core (`src/deflate/**`, `src/stream.rs`, …) is fully
//! safe, and every `unsafe` operation here carries a `// SAFETY:` justification,
//! while every public `unsafe fn` documents its contract in a `# Safety`
//! section.
//!
//! # C integer widths
//!
//! The C integer aliases are taken from [`core::ffi`] (`c_int`, `c_uint`,
//! `c_ulong`, …) rather than `std::os::raw`. The two are the *same* platform-
//! accurate types — `std::os::raw` merely re-exports `core::ffi` — but
//! [`core::ffi`] keeps this module `no_std`-clean. This matters because C
//! `unsigned long` is 64-bit on LP64 targets and 32-bit on Windows/ILP32, so the
//! mirror must never hard-code `u32`/`u64` where the C type is `unsigned long`.

// The types below deliberately mirror zlib's C identifiers verbatim
// (`z_stream`, `uInt`, `alloc_func`, `gzFile`, `z_off64_t`, …) so both the
// emitted symbols and the developer-facing type names match `zlib.h` /
// `zconf.h` exactly — that verbatim spelling is the entire point of an
// ABI-mirror module. The idiomatic upper-camel-case convention is therefore
// intentionally waived for this module only (it does not leak to the rest of
// the crate).
#![allow(non_camel_case_types)]

// The C ABI integer aliases. `core::ffi` provides the exact platform widths
// (identical to `std::os::raw`) while remaining usable under `no_std`.
use core::ffi::{c_char, c_int, c_long, c_uchar, c_uint, c_ulong, c_void};
use core::{ptr, slice};

use alloc::boxed::Box;
use alloc::vec::Vec;

use crate::gz_header::GzHeader;
use crate::stream::{Allocator, DefaultAllocator, ZStream};

// ===========================================================================
// Phase 2 — C scalar type aliases (mirror `zconf.h`)
// ===========================================================================

/// C `Bytef` — a byte (`Byte` = `unsigned char`), the element type of every
/// input/output buffer in the zlib API (`zconf.h`).
pub type Bytef = c_uchar;

/// C `uInt` — `unsigned int` (`zconf.h`). Used for `avail_in`/`avail_out` and
/// most length fields.
pub type uInt = c_uint;

/// C `uLong` — `unsigned long` (`zconf.h`). 64-bit on LP64, 32-bit on
/// Windows/ILP32. Used for `total_in`/`total_out`/`adler`.
pub type uLong = c_ulong;

/// C `uLongf` — a `FAR`-qualified `uLong` (`zconf.h`); identical to [`uLong`] on
/// modern flat-memory targets.
pub type uLongf = c_ulong;

/// C `voidpf` — `void *` (`zconf.h`), the generic mutable pointer zlib uses for
/// allocator buffers and the `opaque` cookie.
pub type voidpf = *mut c_void;

/// C `voidpc` — `const void *` (`zconf.h`).
pub type voidpc = *const c_void;

/// C `voidp` — `void *` (`zconf.h`).
pub type voidp = *mut c_void;

/// C `z_crc_t` — the 32-bit unsigned type used for CRC-32 values (`zconf.h`,
/// `Z_U4`).
pub type z_crc_t = c_uint;

/// C `z_size_t` — `size_t` (`zconf.h`); the element/most-recent length type for
/// the `*_size_t` one-call helpers.
pub type z_size_t = usize;

/// C `z_off_t` — `off_t` (`zconf.h`); the file-offset type for the gz seek API.
/// Maps to `long` on the common Unix configuration.
pub type z_off_t = c_long;

/// C `z_off64_t` — the 64-bit file-offset type (`zconf.h`), used by the `*64`
/// gz entry points and by [`gzFile_s::pos`]. Always 64 bits wide.
pub type z_off64_t = i64;

// ===========================================================================
// Phase 3 — Opaque `internal_state` handle
// ===========================================================================

/// FFI-opaque stand-in for the C `struct internal_state` (`zlib.h` L88), the
/// type of the [`z_stream::state`] field.
///
/// C never constructs or inspects this type — it only ever holds a pointer to
/// it. The Rust FFI stores `Box::into_raw(state) as *mut internal_state` here
/// (see [`state_ptr_from_box`]) and reclaims it with [`state_take`]. The empty,
/// private field makes the type both zero-sized and impossible to construct
/// outside this module, exactly matching the "not visible by applications"
/// contract in `zlib.h`.
#[repr(C)]
pub struct internal_state {
    /// Zero-sized private marker: `internal_state` is opaque and never built in
    /// Rust, only pointed at.
    _private: [u8; 0],
}

// ===========================================================================
// Phase 4 — C function-pointer typedefs
// ===========================================================================
//
// Each is an `Option<unsafe extern "C" fn(…)>`. `Option<fn>` is the ABI-correct
// representation of a *nullable* C function pointer: the null-pointer
// optimization guarantees `Option<extern fn>` has the same size and layout as
// the bare function pointer, with `None` encoded as a null address (validated
// by the tests below). Callers may legitimately leave `zalloc`/`zfree` null.

/// C `alloc_func` — `voidpf (*)(voidpf opaque, uInt items, uInt size)`
/// (`zlib.h` L85). The custom allocation hook a caller may install on
/// [`z_stream::zalloc`].
pub type alloc_func =
    Option<unsafe extern "C" fn(opaque: *mut c_void, items: c_uint, size: c_uint) -> *mut c_void>;

/// C `free_func` — `void (*)(voidpf opaque, voidpf address)` (`zlib.h` L86).
/// The custom deallocation hook a caller may install on [`z_stream::zfree`].
pub type free_func = Option<unsafe extern "C" fn(opaque: *mut c_void, address: *mut c_void)>;

/// C `in_func` — `unsigned (*)(void *, z_const unsigned char **)`
/// (`zlib.h` L1134). The input callback for `inflateBack`.
pub type in_func =
    Option<unsafe extern "C" fn(in_desc: *mut c_void, buf: *mut *const c_uchar) -> c_uint>;

/// C `out_func` — `int (*)(void *, unsigned char *, unsigned)` (`zlib.h`
/// L1136). The output callback for `inflateBack`.
pub type out_func =
    Option<unsafe extern "C" fn(out_desc: *mut c_void, buf: *mut c_uchar, len: c_uint) -> c_int>;

// ===========================================================================
// Phase 5 — `#[repr(C)] z_stream`
// ===========================================================================

/// `#[repr(C)]` mirror of the C `z_stream` (`zlib.h` L90-L110).
///
/// LAYOUT: the field order and widths below are **byte-identical** to `zlib.h`
/// and MUST NOT be reordered — the emitted `cdylib`/`staticlib` presents this
/// exact layout to C consumers. A compile-time guard (below) and unit tests
/// assert the offsets. The idiomatic [`ZStream`](crate::stream::ZStream) is a
/// separate, non-`repr(C)` type; all conversion between the two lives in this
/// module.
///
/// The C `msg` field is `z_const char *`; the mirror stores it as
/// `*mut c_char`. Dropping `const` is ABI-irrelevant (both are a single machine
/// pointer) and lets the shims assign the crate's static, NUL-terminated error
/// strings without a cast.
#[repr(C)]
pub struct z_stream {
    /// Next input byte (C `z_const Bytef *next_in`).
    pub next_in: *const c_uchar,
    /// Number of bytes available at [`next_in`](Self::next_in) (C `uInt`).
    pub avail_in: c_uint,
    /// Total number of input bytes read so far (C `uLong`).
    pub total_in: c_ulong,
    /// Next output byte will go here (C `Bytef *next_out`).
    pub next_out: *mut c_uchar,
    /// Remaining free space at [`next_out`](Self::next_out) (C `uInt`).
    pub avail_out: c_uint,
    /// Total number of bytes output so far (C `uLong`).
    pub total_out: c_ulong,
    /// Last error message, or null when there is no error (C
    /// `z_const char *msg`). Points at a crate-owned static string; never freed
    /// by C.
    pub msg: *mut c_char,
    /// Opaque internal engine state (C `struct internal_state FAR *state`).
    pub state: *mut internal_state,
    /// Caller-supplied allocation hook, or `None` (C `alloc_func zalloc`).
    pub zalloc: alloc_func,
    /// Caller-supplied deallocation hook, or `None` (C `free_func zfree`).
    pub zfree: free_func,
    /// Private cookie passed to [`zalloc`](Self::zalloc)/[`zfree`](Self::zfree)
    /// (C `voidpf opaque`).
    pub opaque: *mut c_void,
    /// Best guess about the data type (C `int data_type`): binary/text for
    /// deflate, or the decode state for inflate.
    pub data_type: c_int,
    /// Adler-32 or CRC-32 of the uncompressed data (C `uLong adler`).
    pub adler: c_ulong,
    /// Reserved for future use (C `uLong reserved`); always `0`.
    pub reserved: c_ulong,
}

/// C `z_streamp` — `z_stream *` (`zlib.h` L112). The handle type every public
/// streaming entry point receives.
pub type z_streamp = *mut z_stream;

// ===========================================================================
// Phase 6 — `#[repr(C)] gz_header`
// ===========================================================================

/// `#[repr(C)]` mirror of the C `gz_header` (`zlib.h` L118-L133), the gzip
/// header exchanged with `deflateSetHeader`/`inflateGetHeader`.
///
/// LAYOUT: field order/widths are byte-identical to `zlib.h`. The idiomatic,
/// owned [`GzHeader`](crate::gz_header::GzHeader) is the safe counterpart;
/// [`gz_header_to_idiomatic`] and [`write_gz_header_from_idiomatic`] convert
/// between the two.
#[repr(C)]
pub struct gz_header {
    /// `true` (non-zero) if the data is believed to be text (C `int text`).
    pub text: c_int,
    /// Modification time (C `uLong time`).
    pub time: c_ulong,
    /// Extra flags — not used when writing (C `int xflags`).
    pub xflags: c_int,
    /// Operating system (C `int os`).
    pub os: c_int,
    /// Pointer to the extra field, or null (C `Bytef *extra`).
    pub extra: *mut c_uchar,
    /// Extra-field length, valid when [`extra`](Self::extra) is non-null
    /// (C `uInt extra_len`).
    pub extra_len: c_uint,
    /// Capacity at [`extra`](Self::extra), used only when reading
    /// (C `uInt extra_max`).
    pub extra_max: c_uint,
    /// Pointer to the zero-terminated file name, or null (C `Bytef *name`).
    pub name: *mut c_uchar,
    /// Capacity at [`name`](Self::name), used only when reading
    /// (C `uInt name_max`).
    pub name_max: c_uint,
    /// Pointer to the zero-terminated comment, or null (C `Bytef *comment`).
    pub comment: *mut c_uchar,
    /// Capacity at [`comment`](Self::comment), used only when reading
    /// (C `uInt comm_max`).
    pub comm_max: c_uint,
    /// `true` (non-zero) if a header CRC is/will be present (C `int hcrc`).
    pub hcrc: c_int,
    /// `true` when done reading the gzip header (C `int done`); the C field is
    /// tri-state (`1` = done, `-1` = raw zlib stream, `0` = still reading).
    pub done: c_int,
}

/// C `gz_headerp` — `gz_header *` (`zlib.h`).
pub type gz_headerp = *mut gz_header;

// ===========================================================================
// Phase 7 — `#[repr(C)] gzFile_s`
// ===========================================================================

/// `#[repr(C)]` mirror of the exposed C `struct gzFile_s` (`zlib.h`
/// L1956-L1960): `{ unsigned have; unsigned char *next; z_off64_t pos; }`.
///
/// zlib exposes just this abbreviated prefix so the C `gzgetc(g)` *macro* can
/// read `have`/`next`/`pos` directly through the [`gzFile`] pointer.
///
/// # Documented limitation
///
/// The crate's idiomatic gz state (`crate::gz::GzState`) is **not** `#[repr(C)]`,
/// so `gz.rs` stores its opaque handle as `Box::into_raw(Box<GzState>) as
/// gzFile` and exposes `gzgetc`/`gzgetc_` as **real functions** rather than
/// relying on the macro's direct field access. Strict macro fast-path parity
/// would require the opaque handle to *begin* with a live `{ have, next, pos }`
/// prefix; that is intentionally not attempted. This struct is still defined
/// here for header/ABI completeness.
#[repr(C)]
pub struct gzFile_s {
    /// Bytes currently available in [`next`](Self::next) (C `unsigned have`).
    pub have: c_uint,
    /// Pointer to the next available byte (C `unsigned char *next`).
    pub next: *mut c_uchar,
    /// Current position in the uncompressed stream (C `z_off64_t pos`).
    pub pos: i64,
}

/// C `gzFile` — `struct gzFile_s *` (`zlib.h`). The opaque handle returned by
/// `gzopen`/`gzdopen`.
pub type gzFile = *mut gzFile_s;

// ===========================================================================
// Phase 8 — Caller-allocator bridge: `CAllocator`
// ===========================================================================

/// Bridges a C caller's `zalloc`/`zfree`/`opaque` triple into the crate's
/// [`Allocator`](crate::stream::Allocator) abstraction (AAP §0.6.3).
///
/// The FFI layer always instantiates its streams as
/// [`ZStream<CAllocator>`](crate::stream::ZStream), giving the deflate/inflate
/// shims a *single* monomorphized handle type to box into
/// [`z_stream::state`] regardless of whether the caller supplied hooks.
///
/// # Known limitation (hooks vs. the `Vec`-based allocator)
///
/// The crate's realized [`Allocator`](crate::stream::Allocator) trait is
/// **`Vec`-based and infallible**: `allocate_zeroed::<T>(count) -> Vec<T>` and
/// `deallocate::<T>(Vec<T>)`. On stable Rust (MSRV 1.85) there is no stable
/// `allocator_api`, so a `Vec` **cannot** be soundly backed by memory obtained
/// from the caller's `zalloc` and later released through `zfree` (dropping the
/// `Vec` would route through the *global* allocator, which is undefined
/// behavior for foreign memory). The trait also has no channel to report a
/// `zalloc` returning null (the C `Z_MEM_ERROR` case). Consequently
/// [`CAllocator`] routes **buffer** allocation through the global allocator —
/// exactly matching AAP §0.6.3 ("otherwise `std::alloc` is used") and the
/// observation that the sibling engines allocate their working buffers
/// (window, `pending_buf`, hash tables) directly via the global allocator.
///
/// The caller's hooks are nonetheless retained on this struct for ABI
/// completeness and so a shim that performs a raw allocation of its own can
/// consult them. The **ABI guarantee** this type upholds is unconditional:
/// supplying `zalloc`/`zfree` never breaks the stream, and leaving them null
/// works.
#[derive(Clone, Copy)]
pub struct CAllocator {
    /// The caller's allocation hook, or `None` (mirrors [`z_stream::zalloc`]).
    pub zalloc: alloc_func,
    /// The caller's deallocation hook, or `None` (mirrors [`z_stream::zfree`]).
    pub zfree: free_func,
    /// The caller's private cookie (mirrors [`z_stream::opaque`]).
    pub opaque: *mut c_void,
}

impl CAllocator {
    /// Captures the `zalloc`/`zfree`/`opaque` triple from a raw [`z_stream`].
    ///
    /// # Safety
    ///
    /// `strm` must reference a validly-initialized [`z_stream`]. Only plain
    /// `Copy` fields are read (no dereference of the hook pointers occurs
    /// here), so the requirement is simply that the reference itself is valid.
    #[inline]
    #[must_use]
    pub unsafe fn from_stream(strm: &z_stream) -> Self {
        // SAFETY: `strm` is a valid `&z_stream`; `zalloc`/`zfree`/`opaque` are
        // plain `Copy` fields (function pointers / a raw cookie pointer) and are
        // merely copied out, never dereferenced.
        Self {
            zalloc: strm.zalloc,
            zfree: strm.zfree,
            opaque: strm.opaque,
        }
    }
}

impl Allocator for CAllocator {
    /// Allocates a zero-initialized buffer of `count` elements.
    ///
    /// Per the [known limitation](CAllocator#known-limitation), this delegates
    /// to the global allocator via [`DefaultAllocator`]; the caller's `zalloc`
    /// hook is not used to back the returned [`Vec`] because a `Vec` cannot be
    /// soundly reclaimed through a foreign `zfree` on stable Rust.
    #[inline]
    fn allocate_zeroed<T>(&self, count: usize) -> Vec<T>
    where
        T: Copy + Default,
    {
        DefaultAllocator.allocate_zeroed(count)
    }

    // `deallocate` intentionally uses the trait default (drops the `Vec` so the
    // global allocator reclaims it). It must NOT route to the caller's `zfree`,
    // because `allocate_zeroed` returned globally-allocated storage.
}

/// Builds an idiomatic [`ZStream<CAllocator>`](crate::stream::ZStream) whose
/// allocator carries the caller's `zalloc`/`zfree`/`opaque` triple.
///
/// This is the single constructor the deflate/inflate init shims use to obtain
/// the one monomorphized handle type they box into [`z_stream::state`]. It
/// honors caller hooks where the design permits and global-allocates otherwise
/// (see [`CAllocator`]).
///
/// # Safety
///
/// `strm` must reference a validly-initialized [`z_stream`] (see
/// [`CAllocator::from_stream`]).
#[inline]
#[must_use]
pub unsafe fn zstream_with_caller_alloc(strm: &z_stream) -> ZStream<CAllocator> {
    // SAFETY: forwarded to the caller: `strm` is a valid `&z_stream`.
    let allocator = unsafe { CAllocator::from_stream(strm) };
    ZStream::with_allocator(allocator)
}

// ===========================================================================
// Phase 9 — Conversion glue (raw ↔ idiomatic)
// ===========================================================================
//
// These helpers are consumed by the sibling shim files (`deflate.rs`,
// `inflate.rs`, `util.rs`, `gz.rs`). They move between the raw `z_stream`/
// `gz_header` and the idiomatic engine state / `GzHeader`. Each raw-pointer
// dereference carries a `// SAFETY:` justification, and each public `unsafe fn`
// documents its contract in a `# Safety` section.

// --- Opaque state-handle helpers -------------------------------------------

/// Consumes a boxed engine handle and returns it as the opaque
/// [`z_stream::state`] pointer.
///
/// The typical `T` is [`ZStream<CAllocator>`](crate::stream::ZStream). Ownership
/// is transferred to the C `state` field; the box must later be reclaimed with
/// [`state_take`] (during `deflateEnd`/`inflateEnd`) to avoid a leak.
///
/// # Safety
///
/// The returned pointer must be treated as owning: it must be reclaimed exactly
/// once via [`state_take`] (or an equivalent `Box::from_raw`), and must not be
/// aliased by another owner.
#[inline]
#[must_use]
pub unsafe fn state_ptr_from_box<T>(b: Box<T>) -> *mut internal_state {
    // `Box::into_raw` is a safe operation; the pointer cast reinterprets the
    // handle as the opaque C `state` type without any dereference.
    Box::into_raw(b) as *mut internal_state
}

/// Borrows the boxed engine handle stored in [`z_stream::state`], or [`None`]
/// if no engine is installed.
///
/// # Safety
///
/// If [`z_stream::state`] is non-null it must point at a live `Box<T>` that was
/// installed by [`state_ptr_from_box`] with the *same* `T`, and must not be
/// mutably aliased for the returned borrow's lifetime (tied to `strm`).
#[inline]
pub unsafe fn state_ref<T>(strm: &mut z_stream) -> Option<&mut T> {
    if strm.state.is_null() {
        None
    } else {
        // SAFETY: `state` is non-null and, per the contract, points at a live,
        // uniquely-owned `T` installed via `state_ptr_from_box::<T>`. The
        // returned borrow is tied to the `&'a mut z_stream`, so it cannot alias.
        Some(unsafe { &mut *(strm.state as *mut T) })
    }
}

/// Reclaims the boxed engine handle from [`z_stream::state`], leaving the field
/// null, or returns [`None`] if no engine is installed.
///
/// Dropping the returned box runs the engine's RAII teardown — the replacement
/// for the C `deflateEnd`/`inflateEnd` free path.
///
/// # Safety
///
/// If [`z_stream::state`] is non-null it must point at a live `Box<T>` (same
/// `T`) installed by [`state_ptr_from_box`], and must not have been reclaimed
/// already (no double free).
#[inline]
pub unsafe fn state_take<T>(strm: &mut z_stream) -> Option<Box<T>> {
    if strm.state.is_null() {
        None
    } else {
        // SAFETY: `state` is non-null and owns a `Box<T>` installed via
        // `state_ptr_from_box::<T>`; reconstituting the box transfers ownership
        // back to Rust exactly once. Null the field to prevent a double free.
        let b = unsafe { Box::from_raw(strm.state as *mut T) };
        strm.state = ptr::null_mut();
        Some(b)
    }
}

// --- Input/output slice bridging -------------------------------------------

/// Views the pending input as a slice of [`z_stream::avail_in`] bytes at
/// [`z_stream::next_in`], or an empty slice when the buffer is null/empty.
///
/// # Safety
///
/// When [`z_stream::next_in`] is non-null and [`z_stream::avail_in`] is
/// non-zero, the caller guarantees `avail_in` bytes are readable at `next_in`
/// and remain valid and unmodified for the returned borrow's lifetime `'a`.
#[inline]
#[must_use]
pub unsafe fn input_slice<'a>(strm: &z_stream) -> &'a [u8] {
    if strm.next_in.is_null() || strm.avail_in == 0 {
        &[]
    } else {
        // SAFETY: per the contract, `avail_in` bytes are readable at the
        // non-null `next_in`. `u8` and `c_uchar` share a layout.
        unsafe { slice::from_raw_parts(strm.next_in, strm.avail_in as usize) }
    }
}

/// Views the free output space as a mutable slice of [`z_stream::avail_out`]
/// bytes at [`z_stream::next_out`], or an empty slice when null/empty.
///
/// # Safety
///
/// When [`z_stream::next_out`] is non-null and [`z_stream::avail_out`] is
/// non-zero, the caller guarantees `avail_out` bytes are writable at `next_out`
/// and that this output region is disjoint from the input region for the
/// returned borrow's lifetime `'a`.
#[inline]
#[must_use]
// The returned `&mut [u8]` aliases the caller's external output buffer
// (`next_out`), which is disjoint from the `z_stream` struct itself; producing
// it from `&z_stream` is sound and lets a shim obtain input and output slices
// from one shared borrow. The `mut_from_ref` heuristic cannot see this.
#[allow(clippy::mut_from_ref)]
pub unsafe fn output_slice<'a>(strm: &z_stream) -> &'a mut [u8] {
    if strm.next_out.is_null() || strm.avail_out == 0 {
        &mut []
    } else {
        // SAFETY: per the contract, `avail_out` bytes are writable at the
        // non-null `next_out` and disjoint from the input region.
        unsafe { slice::from_raw_parts_mut(strm.next_out, strm.avail_out as usize) }
    }
}

/// Advances the input cursor after `consumed` bytes were read: bumps
/// [`z_stream::next_in`], decrements [`z_stream::avail_in`], and accumulates
/// [`z_stream::total_in`].
///
/// # Safety
///
/// `consumed` must not exceed [`z_stream::avail_in`], and the resulting
/// `next_in` must stay within the caller's input buffer.
#[inline]
pub unsafe fn advance_input(strm: &mut z_stream, consumed: usize) {
    // SAFETY: `consumed <= avail_in`, so the offset stays within the input
    // buffer the caller guaranteed for `next_in`.
    strm.next_in = unsafe { strm.next_in.add(consumed) };
    strm.avail_in -= consumed as c_uint;
    strm.total_in = strm.total_in.wrapping_add(consumed as c_ulong);
}

/// Advances the output cursor after `produced` bytes were written: bumps
/// [`z_stream::next_out`], decrements [`z_stream::avail_out`], and accumulates
/// [`z_stream::total_out`].
///
/// # Safety
///
/// `produced` must not exceed [`z_stream::avail_out`], and the resulting
/// `next_out` must stay within the caller's output buffer.
#[inline]
pub unsafe fn advance_output(strm: &mut z_stream, produced: usize) {
    // SAFETY: `produced <= avail_out`, so the offset stays within the output
    // buffer the caller guaranteed for `next_out`.
    strm.next_out = unsafe { strm.next_out.add(produced) };
    strm.avail_out -= produced as c_uint;
    strm.total_out = strm.total_out.wrapping_add(produced as c_ulong);
}

/// Writes the running checksum into [`z_stream::adler`].
#[inline]
pub fn set_adler(strm: &mut z_stream, adler: u32) {
    strm.adler = adler as c_ulong;
}

/// Writes the data-type guess / inflate decode state into
/// [`z_stream::data_type`].
#[inline]
pub fn set_data_type(strm: &mut z_stream, dt: i32) {
    strm.data_type = dt as c_int;
}

/// Points [`z_stream::msg`] at a static, NUL-terminated diagnostic string (or
/// null to clear it).
///
/// The message is a crate-owned `&'static` C string; C must never free it.
#[inline]
pub fn set_msg(strm: &mut z_stream, msg: *const c_char) {
    strm.msg = msg as *mut c_char;
}

// --- `gz_header` conversion ------------------------------------------------

/// Reads a NUL-terminated C string starting at `ptr` into an owned byte vector,
/// **excluding** the terminating NUL.
///
/// # Safety
///
/// `ptr` must be non-null and point at a NUL-terminated sequence of bytes that
/// stays valid for the duration of the read.
unsafe fn cstr_bytes(ptr: *const c_uchar) -> Vec<u8> {
    let mut len = 0usize;
    // SAFETY: per the contract, `ptr` points at a NUL-terminated string, so
    // every `ptr.add(len)` up to and including the terminator is readable.
    while unsafe { *ptr.add(len) } != 0 {
        len += 1;
    }
    // SAFETY: bytes `ptr[0..len]` precede the NUL and are therefore readable.
    unsafe { slice::from_raw_parts(ptr, len) }.to_vec()
}

/// Copies at most `cap` content bytes of `src` to `dst` and NUL-terminates when
/// room remains, mirroring the C `inflateGetHeader` name/comment truncation
/// (the NUL is stored only if the content did not fill the whole capacity).
///
/// # Safety
///
/// `dst` must be non-null and point at a buffer of at least `cap` writable
/// bytes.
unsafe fn write_cstr_bounded(dst: *mut c_uchar, src: &[u8], cap: usize) {
    if cap == 0 {
        return;
    }
    let n = core::cmp::min(src.len(), cap);
    // SAFETY: `dst` has `cap` writable bytes and `n <= cap`, so the copy stays
    // in bounds; `src` and `dst` are distinct buffers.
    unsafe {
        ptr::copy_nonoverlapping(src.as_ptr(), dst, n);
    }
    if n < cap {
        // SAFETY: `n < cap`, so `dst.add(n)` addresses a writable byte within
        // the buffer.
        unsafe {
            *dst.add(n) = 0;
        }
    }
}

/// Converts a raw [`gz_header`] (as passed to `deflateSetHeader`) into the
/// idiomatic [`GzHeader`](crate::gz_header::GzHeader), or [`None`] when `head`
/// is null.
///
/// The [`extra`](gz_header::extra) field is copied using its
/// [`extra_len`](gz_header::extra_len); [`name`](gz_header::name) and
/// [`comment`](gz_header::comment) are read as NUL-terminated C strings (the
/// terminator is dropped). C `int` booleans map to Rust [`bool`].
///
/// # Safety
///
/// `head` must be null or point at a valid [`gz_header`]. When its `extra`
/// pointer is non-null it must be readable for `extra_len` bytes; when `name`/
/// `comment` are non-null they must be NUL-terminated.
#[must_use]
pub unsafe fn gz_header_to_idiomatic(head: *const gz_header) -> Option<GzHeader> {
    if head.is_null() {
        return None;
    }
    // SAFETY: `head` is non-null and, per the contract, points at a valid
    // `gz_header`.
    let h = unsafe { &*head };

    let extra = if h.extra.is_null() {
        None
    } else {
        // SAFETY: a non-null `extra` is readable for `extra_len` bytes
        // (deflateSetHeader contract).
        Some(unsafe { slice::from_raw_parts(h.extra, h.extra_len as usize) }.to_vec())
    };
    let name = if h.name.is_null() {
        None
    } else {
        // SAFETY: a non-null `name` is a NUL-terminated C string.
        Some(unsafe { cstr_bytes(h.name) })
    };
    let comment = if h.comment.is_null() {
        None
    } else {
        // SAFETY: a non-null `comment` is a NUL-terminated C string.
        Some(unsafe { cstr_bytes(h.comment) })
    };

    Some(GzHeader {
        text: h.text != 0,
        time: h.time as u32,
        // `xflags`/`os` are C `int` (== `i32`), assigned directly.
        xflags: h.xflags,
        os: h.os,
        extra,
        name,
        comment,
        hcrc: h.hcrc != 0,
        // C tri-state `done`: only `1` means "header fully read".
        done: h.done == 1,
        // `c_uint` is `u32` on every Rust target, so these assign directly.
        extra_max: h.extra_max,
        name_max: h.name_max,
        comm_max: h.comm_max,
    })
}

/// Writes an idiomatic [`GzHeader`](crate::gz_header::GzHeader) back into a
/// caller-provided raw [`gz_header`] (as used by `inflateGetHeader`), honoring
/// the caller's `extra_max`/`name_max`/`comm_max` capacities and never
/// overrunning the caller's buffers.
///
/// Scalar fields (`text`, `time`, `xflags`, `os`, `hcrc`, `done`) are always
/// written. `extra_len` is set to the *full* source length even when the copy
/// into `extra` is truncated to `extra_max`, matching C `inflate`.
///
/// # Safety
///
/// `head` must be null or point at a valid [`gz_header`]. When its `extra`/
/// `name`/`comment` pointers are non-null, each must address at least
/// `extra_max`/`name_max`/`comm_max` writable bytes respectively.
pub unsafe fn write_gz_header_from_idiomatic(head: *mut gz_header, src: &GzHeader) {
    if head.is_null() {
        return;
    }
    // SAFETY: `head` is non-null and, per the contract, points at a valid,
    // uniquely-borrowed `gz_header` whose buffers are sized by its `*_max`
    // fields.
    let h = unsafe { &mut *head };

    h.text = c_int::from(src.text);
    h.time = src.time as c_ulong;
    // `xflags`/`os` are C `int` (== `i32`), assigned directly.
    h.xflags = src.xflags;
    h.os = src.os;
    h.hcrc = c_int::from(src.hcrc);
    h.done = c_int::from(src.done);

    if let Some(extra) = &src.extra {
        if !h.extra.is_null() {
            let cap = h.extra_max as usize;
            let n = core::cmp::min(cap, extra.len());
            // SAFETY: `h.extra` has `extra_max` writable bytes and `n <= cap`.
            unsafe {
                ptr::copy_nonoverlapping(extra.as_ptr(), h.extra, n);
            }
        }
        // Report the full length even if the copy was truncated (C parity).
        h.extra_len = extra.len() as c_uint;
    }
    if let Some(name) = &src.name
        && !h.name.is_null()
    {
        // SAFETY: `h.name` has `name_max` writable bytes.
        unsafe {
            write_cstr_bounded(h.name, name, h.name_max as usize);
        }
    }
    if let Some(comment) = &src.comment
        && !h.comment.is_null()
    {
        // SAFETY: `h.comment` has `comm_max` writable bytes.
        unsafe {
            write_cstr_bounded(h.comment, comment, h.comm_max as usize);
        }
    }
}

// --- Panic guards ----------------------------------------------------------
//
// A Rust panic must never unwind across the C ABI (that is undefined behavior).
// Every fallible shim body wraps its logic in one of these guards, which run
// the closure under `catch_unwind` (when `std` is available) and substitute a
// safe default return value if it panics. Under `no_std` (where `catch_unwind`
// is unavailable and builds typically use `panic = "abort"`) the closure runs
// directly. The helpers are `pub(crate)` and consumed by the sibling shim
// files, hence `#[allow(dead_code)]` for standalone compilation of this module.

/// Runs `f`, returning its `c_int` result, or `default` if it panics.
#[cfg(feature = "std")]
#[allow(dead_code)]
pub(crate) fn guard_int(
    default: c_int,
    f: impl FnOnce() -> c_int + core::panic::UnwindSafe,
) -> c_int {
    std::panic::catch_unwind(f).unwrap_or(default)
}

/// `no_std` fallback: runs `f` directly (no unwinding to catch).
#[cfg(not(feature = "std"))]
#[allow(dead_code)]
pub(crate) fn guard_int(
    _default: c_int,
    f: impl FnOnce() -> c_int + core::panic::UnwindSafe,
) -> c_int {
    f()
}

/// Runs `f`, returning its `c_ulong` result, or `default` if it panics.
#[cfg(feature = "std")]
#[allow(dead_code)]
pub(crate) fn guard_ulong(
    default: c_ulong,
    f: impl FnOnce() -> c_ulong + core::panic::UnwindSafe,
) -> c_ulong {
    std::panic::catch_unwind(f).unwrap_or(default)
}

/// `no_std` fallback: runs `f` directly (no unwinding to catch).
#[cfg(not(feature = "std"))]
#[allow(dead_code)]
pub(crate) fn guard_ulong(
    _default: c_ulong,
    f: impl FnOnce() -> c_ulong + core::panic::UnwindSafe,
) -> c_ulong {
    f()
}

/// Runs `f`, returning its `*mut T` result, or `default` if it panics.
#[cfg(feature = "std")]
#[allow(dead_code)]
pub(crate) fn guard_ptr<T>(
    default: *mut T,
    f: impl FnOnce() -> *mut T + core::panic::UnwindSafe,
) -> *mut T {
    std::panic::catch_unwind(f).unwrap_or(default)
}

/// `no_std` fallback: runs `f` directly (no unwinding to catch).
#[cfg(not(feature = "std"))]
#[allow(dead_code)]
pub(crate) fn guard_ptr<T>(
    _default: *mut T,
    f: impl FnOnce() -> *mut T + core::panic::UnwindSafe,
) -> *mut T {
    f()
}

/// Runs `f`, returning its [`z_off64_t`] result, or `default` if it panics.
#[cfg(feature = "std")]
#[allow(dead_code)]
pub(crate) fn guard_off(
    default: z_off64_t,
    f: impl FnOnce() -> z_off64_t + core::panic::UnwindSafe,
) -> z_off64_t {
    std::panic::catch_unwind(f).unwrap_or(default)
}

/// `no_std` fallback: runs `f` directly (no unwinding to catch).
#[cfg(not(feature = "std"))]
#[allow(dead_code)]
pub(crate) fn guard_off(
    _default: z_off64_t,
    f: impl FnOnce() -> z_off64_t + core::panic::UnwindSafe,
) -> z_off64_t {
    f()
}

// ===========================================================================
// Phase 10 — Compile-time ABI layout guards
// ===========================================================================
//
// These `const` assertions fail the build if the mirror structs ever lose their
// expected shape. Field-offset ordering is checked exhaustively in the tests
// below via `core::mem::offset_of!`.

const _: () = {
    // The mirror structs must be non-zero-sized aggregates.
    assert!(core::mem::size_of::<z_stream>() > 0);
    assert!(core::mem::size_of::<gz_header>() > 0);

    // `z_stream` begins with `next_in`, a pointer, so it must be pointer-aligned.
    assert!(core::mem::align_of::<z_stream>() == core::mem::align_of::<*const c_void>());

    // `gzFile_s` = { u32 have; ptr next; i64 pos } — at least a pointer plus the
    // 8-byte `pos` beyond `next`'s offset.
    assert!(core::mem::size_of::<gzFile_s>() >= core::mem::size_of::<*mut c_uchar>() + 8);

    // A nullable C function pointer must stay pointer-sized (null-pointer
    // optimization), or the `#[repr(C)]` structs above would not match `libz`.
    assert!(core::mem::size_of::<alloc_func>() == core::mem::size_of::<*const c_void>());
    assert!(core::mem::size_of::<free_func>() == core::mem::size_of::<*const c_void>());
};

// ===========================================================================
// Phase 11 — Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use core::mem::{align_of, offset_of, size_of};

    /// The four C function-pointer typedefs must be pointer-sized, confirming
    /// the null-pointer optimization the `#[repr(C)]` structs rely on.
    #[test]
    fn fn_pointer_typedefs_are_pointer_sized() {
        let ptr = size_of::<*const c_void>();
        assert_eq!(size_of::<alloc_func>(), ptr);
        assert_eq!(size_of::<free_func>(), ptr);
        assert_eq!(size_of::<in_func>(), ptr);
        assert_eq!(size_of::<out_func>(), ptr);
        // `None` for these Options is the null address.
        let a: alloc_func = None;
        assert!(a.is_none());
    }

    /// `z_stream` field offsets must start at 0 and strictly increase in the
    /// declaration order the C ABI mandates.
    #[test]
    fn z_stream_field_offsets_are_ordered() {
        let offsets = [
            offset_of!(z_stream, next_in),
            offset_of!(z_stream, avail_in),
            offset_of!(z_stream, total_in),
            offset_of!(z_stream, next_out),
            offset_of!(z_stream, avail_out),
            offset_of!(z_stream, total_out),
            offset_of!(z_stream, msg),
            offset_of!(z_stream, state),
            offset_of!(z_stream, zalloc),
            offset_of!(z_stream, zfree),
            offset_of!(z_stream, opaque),
            offset_of!(z_stream, data_type),
            offset_of!(z_stream, adler),
            offset_of!(z_stream, reserved),
        ];
        assert_eq!(offsets[0], 0, "next_in must be the first field");
        for pair in offsets.windows(2) {
            assert!(
                pair[1] > pair[0],
                "z_stream fields must be laid out in declaration order"
            );
        }
        // Pointer-aligned aggregate.
        assert_eq!(align_of::<z_stream>(), align_of::<*const c_void>());
    }

    /// `gz_header` field offsets must start at 0 and strictly increase.
    #[test]
    fn gz_header_field_offsets_are_ordered() {
        let offsets = [
            offset_of!(gz_header, text),
            offset_of!(gz_header, time),
            offset_of!(gz_header, xflags),
            offset_of!(gz_header, os),
            offset_of!(gz_header, extra),
            offset_of!(gz_header, extra_len),
            offset_of!(gz_header, extra_max),
            offset_of!(gz_header, name),
            offset_of!(gz_header, name_max),
            offset_of!(gz_header, comment),
            offset_of!(gz_header, comm_max),
            offset_of!(gz_header, hcrc),
            offset_of!(gz_header, done),
        ];
        assert_eq!(offsets[0], 0, "text must be the first field");
        for pair in offsets.windows(2) {
            assert!(pair[1] > pair[0]);
        }
    }

    /// `gzFile_s` exposes `{ have, next, pos }` with `have` first, as the C
    /// `gzgetc` macro requires.
    #[test]
    fn gz_file_s_layout() {
        assert_eq!(offset_of!(gzFile_s, have), 0);
        assert!(offset_of!(gzFile_s, next) >= size_of::<c_uint>());
        assert!(offset_of!(gzFile_s, pos) > offset_of!(gzFile_s, next));
        // Must hold at least a pointer plus the 8-byte `pos`.
        assert!(size_of::<gzFile_s>() >= size_of::<*mut c_uchar>() + 8);
    }

    /// With null hooks, `CAllocator` falls back to the global allocator and
    /// produces zeroed buffers that round-trip through `deallocate` without UB.
    #[test]
    fn callocator_null_hooks_use_global_allocator() {
        let alloc = CAllocator {
            zalloc: None,
            zfree: None,
            opaque: ptr::null_mut(),
        };

        let bytes: Vec<u8> = alloc.allocate_zeroed(8);
        assert_eq!(bytes.len(), 8);
        assert!(bytes.iter().all(|&b| b == 0));
        alloc.deallocate(bytes);

        let words: Vec<u32> = alloc.allocate_zeroed(4);
        assert_eq!(words, alloc::vec![0u32; 4]);
        alloc.deallocate(words);

        // A zero-length request yields an empty buffer.
        let empty: Vec<u16> = alloc.allocate_zeroed(0);
        assert!(empty.is_empty());
        alloc.deallocate(empty);
    }

    /// `zstream_with_caller_alloc` produces a `ZStream<CAllocator>` carrying the
    /// caller's hooks/cookie.
    #[test]
    fn zstream_with_caller_alloc_carries_hooks() {
        let cookie = 0xABCD_usize as *mut c_void;
        let strm = z_stream {
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
            opaque: cookie,
            data_type: 0,
            adler: 0,
            reserved: 0,
        };
        // SAFETY: `strm` is a fully-initialized local `z_stream`.
        let z = unsafe { zstream_with_caller_alloc(&strm) };
        assert_eq!(z.allocator().opaque, cookie);
        assert!(!z.has_state());
    }

    /// The state-handle helpers round-trip a boxed value through the opaque
    /// `state` pointer.
    #[test]
    fn state_handle_round_trip() {
        let mut strm = zeroed_stream();
        assert!(strm.state.is_null());

        // Install a boxed value.
        let boxed = Box::new(1234u64);
        // SAFETY: transfers ownership of a freshly boxed value into `state`.
        strm.state = unsafe { state_ptr_from_box(boxed) };
        assert!(!strm.state.is_null());

        // Borrow it back.
        // SAFETY: `state` holds a live `Box<u64>` installed just above.
        let borrowed: Option<&mut u64> = unsafe { state_ref::<u64>(&mut strm) };
        assert_eq!(borrowed.copied(), Some(1234));

        // Reclaim it; the field is nulled and the box drops here.
        // SAFETY: `state` still holds the same live `Box<u64>`.
        let taken: Option<Box<u64>> = unsafe { state_take::<u64>(&mut strm) };
        assert_eq!(taken.as_deref().copied(), Some(1234));
        assert!(strm.state.is_null());

        // A second take yields `None`.
        // SAFETY: `state` is now null.
        let again = unsafe { state_take::<u64>(&mut strm) };
        assert!(again.is_none());
    }

    /// Input/output slice bridging returns empty slices for null/zero cursors
    /// and correct views otherwise.
    #[test]
    fn slice_bridging_and_cursor_advance() {
        let input = [10u8, 20, 30, 40];
        let mut output = [0u8; 4];

        let mut strm = zeroed_stream();
        strm.next_in = input.as_ptr();
        strm.avail_in = input.len() as c_uint;
        strm.next_out = output.as_mut_ptr();
        strm.avail_out = output.len() as c_uint;

        // SAFETY: cursors point at the live local buffers with matching lengths.
        let inp = unsafe { input_slice(&strm) };
        assert_eq!(inp, &[10, 20, 30, 40]);
        // SAFETY: as above; the output buffer is disjoint from the input.
        let out = unsafe { output_slice(&strm) };
        out.copy_from_slice(&[1, 2, 3, 4]);

        // Advance both cursors by 2.
        // SAFETY: 2 <= avail_in and 2 <= avail_out.
        unsafe {
            advance_input(&mut strm, 2);
            advance_output(&mut strm, 2);
        }
        assert_eq!(strm.avail_in, 2);
        assert_eq!(strm.total_in, 2);
        assert_eq!(strm.avail_out, 2);
        assert_eq!(strm.total_out, 2);
        assert_eq!(output, [1, 2, 3, 4]);

        // Null/zero cursors yield empty slices.
        let empty = zeroed_stream();
        // SAFETY: both cursors are null.
        assert!(unsafe { input_slice(&empty) }.is_empty());
        // SAFETY: both cursors are null.
        assert!(unsafe { output_slice(&empty) }.is_empty());
    }

    /// Setters write the raw fields with the correct C widths.
    #[test]
    fn field_setters() {
        let mut strm = zeroed_stream();
        let adler: u32 = 0xDEAD_BEEF;
        set_adler(&mut strm, adler);
        set_data_type(&mut strm, 1);
        assert_eq!(strm.adler, c_ulong::from(adler));
        assert_eq!(strm.data_type, 1);

        let msg = c"boom";
        set_msg(&mut strm, msg.as_ptr());
        assert_eq!(strm.msg as *const c_char, msg.as_ptr());
    }

    /// A `gz_header` converts to `GzHeader` and back with field equivalence for
    /// the scalars and the `extra`/`name`/`comment` byte vectors.
    #[test]
    fn gz_header_round_trip() {
        let mut extra_src = [1u8, 2, 3];
        let mut name_src = *b"file.txt\0";
        let mut comment_src = *b"cmt\0";

        let src = gz_header {
            text: 1,
            time: 0x1234_5678,
            xflags: 7,
            os: 3,
            extra: extra_src.as_mut_ptr(),
            extra_len: 3,
            extra_max: 0,
            name: name_src.as_mut_ptr(),
            name_max: 0,
            comment: comment_src.as_mut_ptr(),
            comm_max: 0,
            hcrc: 1,
            done: 0,
        };

        // SAFETY: `src` is fully initialized; `extra` is readable for
        // `extra_len` bytes and `name`/`comment` are NUL-terminated.
        let idi = unsafe { gz_header_to_idiomatic(&src) }.expect("non-null header");
        assert!(idi.text);
        assert_eq!(idi.time, 0x1234_5678);
        assert_eq!(idi.xflags, 7);
        assert_eq!(idi.os, 3);
        assert_eq!(idi.extra.as_deref(), Some(&[1u8, 2, 3][..]));
        assert_eq!(idi.name.as_deref(), Some(&b"file.txt"[..]));
        assert_eq!(idi.comment.as_deref(), Some(&b"cmt"[..]));
        assert!(idi.hcrc);

        // Write back into caller-owned buffers.
        let mut extra_dst = [0u8; 8];
        let mut name_dst = [0u8; 16];
        let mut comment_dst = [0u8; 16];
        let mut out = gz_header {
            text: 0,
            time: 0,
            xflags: 0,
            os: 0,
            extra: extra_dst.as_mut_ptr(),
            extra_len: 0,
            extra_max: extra_dst.len() as c_uint,
            name: name_dst.as_mut_ptr(),
            name_max: name_dst.len() as c_uint,
            comment: comment_dst.as_mut_ptr(),
            comm_max: comment_dst.len() as c_uint,
            hcrc: 0,
            done: 0,
        };
        // SAFETY: `out`'s buffers are sized by its `*_max` fields.
        unsafe { write_gz_header_from_idiomatic(&mut out, &idi) };

        assert_eq!(out.text, 1);
        assert_eq!(out.time, 0x1234_5678 as c_ulong);
        assert_eq!(out.xflags, 7);
        assert_eq!(out.os, 3);
        assert_eq!(out.hcrc, 1);
        assert_eq!(out.extra_len, 3);
        assert_eq!(&extra_dst[..3], &[1, 2, 3]);
        assert_eq!(&name_dst[..8], b"file.txt");
        assert_eq!(name_dst[8], 0, "name must be NUL-terminated");
        assert_eq!(&comment_dst[..3], b"cmt");
        assert_eq!(comment_dst[3], 0, "comment must be NUL-terminated");
    }

    /// A null `gz_header` pointer converts to `None`, and writing back through a
    /// null pointer is a no-op.
    #[test]
    fn gz_header_null_is_none() {
        // SAFETY: the null case is handled without dereferencing.
        assert!(unsafe { gz_header_to_idiomatic(ptr::null()) }.is_none());
        let hdr = GzHeader::new();
        // SAFETY: writing through null is an explicit no-op.
        unsafe { write_gz_header_from_idiomatic(ptr::null_mut(), &hdr) };
    }

    /// `write_gz_header_from_idiomatic` truncates to the caller's capacities and
    /// never overruns.
    #[test]
    fn write_gz_header_truncates_to_capacity() {
        let src = GzHeader::new()
            .with_name(b"toolongname".to_vec())
            .with_extra(alloc::vec![9u8; 10]);

        let mut name_dst = [0xAAu8; 4];
        let mut extra_dst = [0xAAu8; 4];
        let mut out = gz_header {
            text: 0,
            time: 0,
            xflags: 0,
            os: 0,
            extra: extra_dst.as_mut_ptr(),
            extra_len: 0,
            extra_max: extra_dst.len() as c_uint,
            name: name_dst.as_mut_ptr(),
            name_max: name_dst.len() as c_uint,
            comment: ptr::null_mut(),
            comm_max: 0,
            hcrc: 0,
            done: 0,
        };
        // SAFETY: buffers are sized by the `*_max` fields (4 bytes each).
        unsafe { write_gz_header_from_idiomatic(&mut out, &src) };

        // name: filled to the full 4-byte capacity with content and NO
        // terminator. This mirrors C `inflateGetHeader`, which truncates the
        // name to exactly `name_max` *data* bytes and only stores the NUL when
        // the name (plus its terminator) fits within the capacity.
        assert_eq!(&name_dst[..4], b"tool");
        // extra: filled to capacity (no room for a terminator; extra is binary).
        assert_eq!(extra_dst, [9, 9, 9, 9]);
        // extra_len reports the full source length even though the copy was cut.
        assert_eq!(out.extra_len, 10);
    }

    /// The panic guard substitutes the default value when the body panics and
    /// passes the value through otherwise.
    #[cfg(feature = "std")]
    #[test]
    fn guard_int_catches_panic_and_passes_value() {
        assert_eq!(guard_int(-2, || 7), 7);

        // Suppress the default panic hook so the test log stays clean.
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let caught = guard_int(-2, || panic!("boundary panic"));
        std::panic::set_hook(prev);
        assert_eq!(caught, -2);
    }

    // -- test helpers -------------------------------------------------------

    /// Builds a fully-zeroed `z_stream` for tests (the state a C caller would
    /// `memset` before `deflateInit`/`inflateInit`).
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
}
