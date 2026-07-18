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
//!   [`Allocator`] abstraction.
//! * **Raw ↔ idiomatic conversion helpers** that the deflate/inflate/util shims
//!   use to move between the raw [`z_stream`]/[`gz_header`] and the idiomatic
//!   [`ZStream`]/[`GzHeader`].
//! * **Panic guards** (`guard_int`, …) so a shim body can never unwind across
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
use crate::stream::{AllocBuffer, AllocHook, Allocator, ZStream};

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
/// assert the offsets. The idiomatic [`ZStream`] is a
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
/// owned [`GzHeader`] is the safe counterpart;
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
/// # Live prefix (C2)
///
/// The crate's idiomatic gz state (`crate::gz::GzState`) is **not** `#[repr(C)]`,
/// so `gz.rs` boxes its opaque handle as a `#[repr(C)]` `GzHandle` whose FIRST
/// field is a live instance of this struct. The `gzFile` pointer therefore
/// begins with a real `{ have, next, pos }` prefix at offset 0 that the C
/// `gzgetc(g)` macro can read and advance directly; `gz.rs` reconciles the
/// prefix with the idiomatic cursor on entry to every shim and re-syncs it on
/// exit, giving true macro fast-path parity. `gzgetc`/`gzgetc_` remain exported
/// as real functions for callers that take the function pointer.
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
/// [`Allocator`] abstraction (AAP §0.6.3).
///
/// The FFI layer always instantiates its streams as
/// [`ZStream<CAllocator>`](crate::stream::ZStream), giving the deflate/inflate
/// shims a *single* monomorphized handle type to box into
/// [`z_stream::state`] regardless of whether the caller supplied hooks.
///
/// # Hook routing (AAP §0.6.3 "has-hook clause")
///
/// The crate's [`Allocator`] trait returns an
/// [`AllocBuffer`] — a smart owned region that is
/// **either** a global-allocator [`Vec`] **or** a foreign region carved from a
/// caller's `zalloc` and released through their `zfree` on [`Drop`]. This
/// resolves the historical limitation (a `Vec` cannot be soundly reclaimed
/// through a foreign `zfree` on stable Rust, which lacks a stable
/// `allocator_api`): the foreign case is a dedicated variant that owns its raw
/// pointer and hook, so its `Drop` calls the matching `zfree` — never the
/// global allocator.
///
/// [`CAllocator`] therefore forwards its captured triple as an
/// [`AllocHook`] via [`Allocator::hook`], and its
/// [`allocate_zeroed`](Allocator::allocate_zeroed) routes every working buffer
/// (window, `pending_buf`, hash tables) through the caller's `zalloc` when
/// **both** `zalloc` and `zfree` are supplied. When they are null (or `zalloc`
/// reports OOM) it falls back to the global allocator, exactly matching AAP
/// §0.6.3's "otherwise `std::alloc` is used" clause. The **ABI guarantee** is
/// unconditional: supplying `zalloc`/`zfree` routes allocation through them and
/// never breaks the stream, and leaving them null works identically to before.
///
/// The boxed engine *state* handle itself is still allocated through the global
/// allocator (Rust `Box`); it is the working *buffers* — the bulk of a stream's
/// footprint and precisely what AAP §0.6.3 enumerates — that honor the hook.
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
    /// Allocates a zero-initialized buffer of `count` elements, **routing
    /// through the caller's `zalloc` when supplied** (AAP §0.6.3, "has-hook
    /// clause").
    ///
    /// The returned [`AllocBuffer`] is a [`Foreign`](AllocBuffer::Foreign)
    /// region carved from the caller's `zalloc` (and released through their
    /// `zfree` on drop) whenever this [`CAllocator`] carries an active
    /// [`hook`](Allocator::hook); otherwise — null hooks, an empty request, or a
    /// `zalloc` reporting OOM — it falls back to a global-allocator
    /// [`Vec`](AllocBuffer::Owned), matching AAP §0.6.3's "otherwise `std::alloc`
    /// is used" clause. Soundness is preserved because the returned buffer knows
    /// its own backing store and frees it the matching way on [`Drop`] — the
    /// historical unsoundness of dropping a foreign-backed `Vec` through the
    /// global allocator cannot occur.
    #[inline]
    fn allocate_zeroed<T>(&self, count: usize) -> Option<AllocBuffer<T>>
    where
        T: Copy + Default + 'static,
    {
        let hook = self.hook();
        // Null hooks / empty requests use the global allocator, matching AAP
        // §0.6.3's "otherwise `std::alloc` is used" clause.
        if !hook.is_active() || count == 0 {
            return Some(AllocBuffer::Owned(alloc::vec![T::default(); count]));
        }
        // Active hook: build a foreign region through the sanctioned in-`ffi`
        // allocator. This calls the confined raw-pointer bridge directly (a
        // legal intra-`ffi` call) rather than routing back through the safe
        // core's `AllocBuffer::try_zeroed`, so this shim serves every
        // `Copy + Default + 'static` element type — not only the engine
        // `ForeignElem` set — while all `unsafe` stays inside `crate::ffi`.
        // `None` (OOM / unrepresentable size) propagates as `Z_MEM_ERROR` (M7).
        super::alloc::try_alloc_foreign::<T>(hook, count).map(AllocBuffer::Foreign)
    }

    /// Exposes the caller's `zalloc`/`zfree`/`opaque` triple as an
    /// [`AllocHook`], so buffers this allocator produces (directly, or lazily on
    /// a state it initializes) are backed by the caller's allocator.
    ///
    /// The hook carries the `ffi`-owned [`FOREIGN_VTABLE`](super::alloc::FOREIGN_VTABLE)
    /// so the safe core can build foreign buffers through it without ever naming
    /// `crate::ffi` (AAP §0.6.1 dependency inversion).
    #[inline]
    fn hook(&self) -> AllocHook {
        AllocHook::new(self.zalloc, self.zfree, self.opaque)
            .with_vtable(&super::alloc::FOREIGN_VTABLE)
    }

    // `deallocate` uses the trait default: dropping the `AllocBuffer` routes to
    // the correct deallocator — the caller's `zfree` for a `Foreign` region, or
    // the global allocator for an `Owned` fallback.
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

// --- Type-tagged state handles (FINDING-6) ---------------------------------
//
// The C `deflateEnd`/`inflateEnd` contract lets a caller (incorrectly) pass a
// stream that was initialized by the *other* engine. The deflate and inflate
// handles are `Box`es of DIFFERENT concrete types — hence different sizes — so a
// blind `Box::from_raw(state as *mut T)` inside the wrong `End` shim would
// reconstitute (and drop) a `Box<T>` whose `Layout` does not match the original
// allocation. That violates the `GlobalAlloc::dealloc` contract (the dealloc
// `Layout` must equal the alloc `Layout`): undefined behavior, benign on glibc's
// size-agnostic `free` but heap-corrupting under sized-dealloc allocators such
// as jemalloc/mimalloc. Tagging each handle with a discriminant at offset 0 and
// validating it BEFORE reconstituting the box makes the mismatch detectable, so
// the shim returns `Z_STREAM_ERROR` without ever dropping a wrong-type box.

/// Discriminant magic stored as the FIRST field (offset 0) of every boxed FFI
/// engine handle installed in [`z_stream::state`].
///
/// Stored as a plain `u64` (via `#[repr(transparent)]`) so the tag can be read
/// through `HandleHeader` with no enum-validity concern: every bit pattern is
/// a valid `u64`, and only the published magics compare equal. The values have
/// their high bits set so they can never collide with the small integer that
/// leads a bare engine state, providing defense-in-depth even though every FFI
/// init shim now installs a tagged `#[repr(C)]` handle.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(transparent)]
pub struct HandleKind(u64);

impl HandleKind {
    /// Tag identifying a boxed [`DeflateHandle`] (a `deflate*`-owned stream).
    pub const DEFLATE: HandleKind = HandleKind(0xDEF1_A7E5_0DEF_0001);
    /// Tag identifying a boxed inflate handle (an `inflate*`-owned stream).
    pub const INFLATE: HandleKind = HandleKind(0x14F1_A7E5_0114_0002);
    /// Tag identifying a boxed `inflateBack` handle (an `inflateBack*`-owned
    /// stream). Distinct from [`INFLATE`](Self::INFLATE) so the regular
    /// `inflateEnd`/`inflateBackEnd` terminators can reject cross-type misuse
    /// (freeing an `inflateBack` state as a plain inflate handle, or vice
    /// versa, would be a layout-mismatched free — the same UB class as C4).
    pub const INFLATE_BACK: HandleKind = HandleKind(0x14F1_A7E5_BAC6_0003);
}

/// The common `#[repr(C)]` prefix shared by every tagged handle: a
/// [`HandleKind`] at offset 0, readable regardless of the concrete handle type
/// behind the opaque `state` pointer.
#[repr(C)]
struct HandleHeader {
    kind: HandleKind,
}

/// The boxed deflate engine handle installed in [`z_stream::state`] by the
/// `deflate*` FFI shims.
///
/// Wrapping [`ZStream<CAllocator>`](crate::stream::ZStream) behind a
/// [`HandleKind`] tag lets [`deflate_take`]/[`deflate_state`] confirm the handle
/// really is a deflate handle before reinterpreting the opaque pointer,
/// preventing the layout-mismatched deallocation that a blind cast would cause
/// on cross-type `End` misuse (FINDING-6). `#[repr(C)]` guarantees `kind` is at
/// offset 0.
#[repr(C)]
pub struct DeflateHandle {
    /// Discriminant tag; always [`HandleKind::DEFLATE`]. MUST be the first field.
    kind: HandleKind,
    /// The idiomatic deflate stream this handle owns.
    pub zs: ZStream<CAllocator>,
}

impl DeflateHandle {
    /// Wraps a freshly built deflate stream, tagging it as a deflate handle.
    #[inline]
    #[must_use]
    pub fn new(zs: ZStream<CAllocator>) -> Self {
        Self {
            kind: HandleKind::DEFLATE,
            zs,
        }
    }
}

/// Reads the [`HandleKind`] tag at offset 0 of the installed state handle, or
/// [`None`] if no handle is installed.
///
/// # Safety
///
/// If [`z_stream::state`] is non-null it must point at a live boxed handle whose
/// first field is a [`HandleKind`]. Every FFI init shim — `deflateInit*`,
/// `inflateInit*`, and `inflateBackInit_` — boxes a `#[repr(C)]` type with a
/// leading `HandleKind`, so the tag is always present and simply read/compared
/// (no enum-validity requirement).
#[inline]
#[must_use]
pub unsafe fn peek_handle_kind(strm: &z_stream) -> Option<HandleKind> {
    if strm.state.is_null() {
        None
    } else {
        // SAFETY: `state` is non-null and points at a live handle allocation of
        // at least 8 bytes; reading the leading `HandleKind` (a `u64`) is valid
        // for any such allocation and carries no enum-validity requirement.
        Some(unsafe { (*(strm.state as *const HandleHeader)).kind })
    }
}

/// Borrows the deflate engine behind the state handle, validating the handle
/// kind first. Returns [`None`] when no handle is installed OR the installed
/// handle is not a deflate handle, letting the caller return `Z_STREAM_ERROR`
/// WITHOUT reinterpreting a wrong-type allocation.
///
/// # Safety
///
/// A non-null [`z_stream::state`] must point at a live handle installed by an
/// FFI init shim (so its leading tag is readable).
#[inline]
pub unsafe fn deflate_state(strm: &mut z_stream) -> Option<&mut ZStream<CAllocator>> {
    // SAFETY: delegated tag read; see `peek_handle_kind`.
    match unsafe { peek_handle_kind(strm) } {
        Some(kind) if kind == HandleKind::DEFLATE => {
            // SAFETY: the tag confirms a live `DeflateHandle`; the borrow is tied
            // to `strm`, so it cannot alias for its lifetime.
            let handle = unsafe { &mut *(strm.state as *mut DeflateHandle) };
            Some(&mut handle.zs)
        }
        _ => None,
    }
}

/// Reclaims the boxed [`DeflateHandle`] from the state field, validating the
/// kind first and nulling `state` on success. Returns [`None`] — leaving
/// `state` untouched — when the handle is absent or not a deflate handle, so
/// `deflateEnd` never drops a wrong-type box.
///
/// # Safety
///
/// A non-null [`z_stream::state`] must point at a live handle installed by an
/// FFI init shim, not already reclaimed.
#[inline]
pub unsafe fn deflate_take(strm: &mut z_stream) -> Option<Box<DeflateHandle>> {
    // SAFETY: delegated tag read; see `peek_handle_kind`.
    match unsafe { peek_handle_kind(strm) } {
        Some(kind) if kind == HandleKind::DEFLATE => {
            // SAFETY: the tag confirms a live `Box<DeflateHandle>`; reconstitute
            // exactly once and null the field to prevent a double free.
            let boxed = unsafe { Box::from_raw(strm.state as *mut DeflateHandle) };
            strm.state = ptr::null_mut();
            Some(boxed)
        }
        _ => None,
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

/// Returns `true` when the input pointer is consistent with `avail_in`: a
/// positive `avail_in` requires a non-null `next_in` (a null `next_in` is only
/// permitted when `avail_in == 0`).
///
/// This is the input half of the entry-validation contract, factored out for
/// the call sites that constrain only the input buffer — e.g. C `inflateSync`
/// (`inflate.c`), which validates `next_in`/`avail_in` but has no output buffer
/// to check.
///
/// It is a *pure* pointer/length consistency check performed **before** any
/// bridging via [`input_slice`]: that primitive deliberately tolerates a
/// null-and-empty input by yielding an empty slice, so the shims must reject
/// the `avail_in != 0 && next_in == NULL` programmer error explicitly here
/// rather than let it be silently masked into an empty read.
#[inline]
#[must_use]
pub fn input_ptr_valid(strm: &z_stream) -> bool {
    strm.avail_in == 0 || !strm.next_in.is_null()
}

/// Returns `true` when the stream's I/O buffers satisfy the entry-validation
/// contract shared by C `deflate` and `inflate`: `next_out` must be non-null,
/// and a positive `avail_in` requires a non-null `next_in`.
///
/// This mirrors the guards at the top of C `deflate` (`deflate.c` L981-L1010)
/// and C `inflate` (`inflate.c` L474). A null `next_out` is rejected
/// **unconditionally** — matching C, whose `next_out == Z_NULL` check carries
/// no `avail_out` qualifier — because [`output_slice`] would otherwise mask a
/// null-and-empty output buffer into an empty slice. Either violation is a
/// programmer error the shims report as `Z_STREAM_ERROR` *before* any buffer is
/// dereferenced.
#[inline]
#[must_use]
pub fn stream_buffers_valid(strm: &z_stream) -> bool {
    !strm.next_out.is_null() && input_ptr_valid(strm)
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
/// idiomatic [`GzHeader`], or [`None`] when `head`
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

/// Writes an idiomatic [`GzHeader`] back into a
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
    if let Some(name) = &src.name {
        // Nested `if` rather than an `if let ... && ...` chain, which is
        // unstable before Rust 1.88 (this crate's MSRV is 1.85).
        if !h.name.is_null() {
            // SAFETY: `h.name` has `name_max` writable bytes.
            unsafe {
                write_cstr_bounded(h.name, name, h.name_max as usize);
            }
        }
    }
    if let Some(comment) = &src.comment {
        // Nested `if` rather than an `if let ... && ...` chain, which is
        // unstable before Rust 1.88 (this crate's MSRV is 1.85).
        if !h.comment.is_null() {
            // SAFETY: `h.comment` has `comm_max` writable bytes.
            unsafe {
                write_cstr_bounded(h.comment, comment, h.comm_max as usize);
            }
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

// Exact numeric ABI layout guard for the primary supported target family:
// 64-bit **LP64** (Linux/macOS on `x86_64`/`aarch64`), where `c_ulong` (C
// `uLong`) is 8 bytes. On this ABI the mirror structs must match `libz`'s
// `z_stream`/`gz_header`/`gzFile_s` byte for byte, so every field offset and the
// total size is pinned below; any drift fails the build (Min1 hardening).
//
// Windows x64 (LLP64, `c_ulong` = 4) and 32-bit targets carry a different
// `uLong` width and therefore different offsets, so they are intentionally
// excluded from these *exact* checks — the ABI-portable ordering/size tests in
// the test module still apply to every target.
#[cfg(all(target_pointer_width = "64", not(windows)))]
const _: () = {
    use core::mem::{offset_of, size_of};

    // `z_stream` — 14 fields, 112 bytes total on LP64.
    assert!(offset_of!(z_stream, next_in) == 0);
    assert!(offset_of!(z_stream, avail_in) == 8);
    assert!(offset_of!(z_stream, total_in) == 16);
    assert!(offset_of!(z_stream, next_out) == 24);
    assert!(offset_of!(z_stream, avail_out) == 32);
    assert!(offset_of!(z_stream, total_out) == 40);
    assert!(offset_of!(z_stream, msg) == 48);
    assert!(offset_of!(z_stream, state) == 56);
    assert!(offset_of!(z_stream, zalloc) == 64);
    assert!(offset_of!(z_stream, zfree) == 72);
    assert!(offset_of!(z_stream, opaque) == 80);
    assert!(offset_of!(z_stream, data_type) == 88);
    assert!(offset_of!(z_stream, adler) == 96);
    assert!(offset_of!(z_stream, reserved) == 104);
    assert!(size_of::<z_stream>() == 112);

    // `gz_header` — 13 fields, 80 bytes total on LP64.
    assert!(offset_of!(gz_header, text) == 0);
    assert!(offset_of!(gz_header, time) == 8);
    assert!(offset_of!(gz_header, xflags) == 16);
    assert!(offset_of!(gz_header, os) == 20);
    assert!(offset_of!(gz_header, extra) == 24);
    assert!(offset_of!(gz_header, extra_len) == 32);
    assert!(offset_of!(gz_header, extra_max) == 36);
    assert!(offset_of!(gz_header, name) == 40);
    assert!(offset_of!(gz_header, name_max) == 48);
    assert!(offset_of!(gz_header, comment) == 56);
    assert!(offset_of!(gz_header, comm_max) == 64);
    assert!(offset_of!(gz_header, hcrc) == 68);
    assert!(offset_of!(gz_header, done) == 72);
    assert!(size_of::<gz_header>() == 80);

    // `gzFile_s` — `{ have, next, pos }`, 24 bytes total on LP64. This is the
    // live prefix the C `gzgetc(g)` macro dereferences (C2), so its exact shape
    // matters for drop-in macro consumers.
    assert!(offset_of!(gzFile_s, have) == 0);
    assert!(offset_of!(gzFile_s, next) == 8);
    assert!(offset_of!(gzFile_s, pos) == 16);
    assert!(size_of::<gzFile_s>() == 24);
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

    /// Min1: **exact** numeric field offsets and total sizes on the supported
    /// 64-bit LP64 ABI (Linux/macOS on `x86_64`/`aarch64`), pinning the mirror
    /// structs to `libz`'s byte-for-byte layout. The compile-time `const _`
    /// guard in the module enforces the identical values at build time; this
    /// test surfaces them as visible coverage. Excluded on Windows LLP64
    /// (`c_ulong` = 4) and 32-bit targets, whose `uLong` width shifts offsets.
    #[cfg(all(target_pointer_width = "64", not(windows)))]
    #[test]
    fn abi_exact_field_offsets_lp64() {
        // z_stream — 112 bytes.
        assert_eq!(offset_of!(z_stream, next_in), 0);
        assert_eq!(offset_of!(z_stream, avail_in), 8);
        assert_eq!(offset_of!(z_stream, total_in), 16);
        assert_eq!(offset_of!(z_stream, next_out), 24);
        assert_eq!(offset_of!(z_stream, avail_out), 32);
        assert_eq!(offset_of!(z_stream, total_out), 40);
        assert_eq!(offset_of!(z_stream, msg), 48);
        assert_eq!(offset_of!(z_stream, state), 56);
        assert_eq!(offset_of!(z_stream, zalloc), 64);
        assert_eq!(offset_of!(z_stream, zfree), 72);
        assert_eq!(offset_of!(z_stream, opaque), 80);
        assert_eq!(offset_of!(z_stream, data_type), 88);
        assert_eq!(offset_of!(z_stream, adler), 96);
        assert_eq!(offset_of!(z_stream, reserved), 104);
        assert_eq!(size_of::<z_stream>(), 112);

        // gz_header — 80 bytes.
        assert_eq!(offset_of!(gz_header, text), 0);
        assert_eq!(offset_of!(gz_header, time), 8);
        assert_eq!(offset_of!(gz_header, xflags), 16);
        assert_eq!(offset_of!(gz_header, os), 20);
        assert_eq!(offset_of!(gz_header, extra), 24);
        assert_eq!(offset_of!(gz_header, extra_len), 32);
        assert_eq!(offset_of!(gz_header, extra_max), 36);
        assert_eq!(offset_of!(gz_header, name), 40);
        assert_eq!(offset_of!(gz_header, name_max), 48);
        assert_eq!(offset_of!(gz_header, comment), 56);
        assert_eq!(offset_of!(gz_header, comm_max), 64);
        assert_eq!(offset_of!(gz_header, hcrc), 68);
        assert_eq!(offset_of!(gz_header, done), 72);
        assert_eq!(size_of::<gz_header>(), 80);

        // gzFile_s — 24 bytes; the live `gzgetc` macro prefix (C2).
        assert_eq!(offset_of!(gzFile_s, have), 0);
        assert_eq!(offset_of!(gzFile_s, next), 8);
        assert_eq!(offset_of!(gzFile_s, pos), 16);
        assert_eq!(size_of::<gzFile_s>(), 24);
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

        let bytes: AllocBuffer<u8> = alloc
            .allocate_zeroed(8)
            .expect("global allocation is infallible");
        assert_eq!(bytes.len(), 8);
        assert!(bytes.iter().all(|&b| b == 0));
        alloc.deallocate(bytes);

        let words: AllocBuffer<u32> = alloc
            .allocate_zeroed(4)
            .expect("global allocation is infallible");
        assert_eq!(&words[..], &[0u32; 4][..]);
        alloc.deallocate(words);

        // A zero-length request yields an empty buffer.
        let empty: AllocBuffer<u16> = alloc
            .allocate_zeroed(0)
            .expect("global allocation is infallible");
        assert!(empty.is_empty());
        alloc.deallocate(empty);
    }

    /// With active hooks, `CAllocator::allocate_zeroed` routes through the
    /// caller's `zalloc` (a `Foreign` buffer, zero-filled and usable as a
    /// slice) and releases it through the caller's `zfree` on drop — the
    /// FINDING-3 has-hook clause. Invocation counts are recorded via a global
    /// (`static`) counter because the C hooks receive only the `opaque` cookie.
    #[test]
    fn callocator_active_hooks_are_invoked_and_balanced() {
        use core::sync::atomic::{AtomicUsize, Ordering};

        static ALLOCS: AtomicUsize = AtomicUsize::new(0);
        static FREES: AtomicUsize = AtomicUsize::new(0);

        unsafe extern "C" fn zalloc(
            _opaque: *mut c_void,
            items: c_uint,
            size: c_uint,
        ) -> *mut c_void {
            ALLOCS.fetch_add(1, Ordering::SeqCst);
            let bytes = (items as usize) * (size as usize);
            // Delegate the actual bytes to the Rust global allocator; a
            // `Vec<u8>` we forget and later reclaim in `zfree`.
            let mut v = alloc::vec![0u8; bytes.max(1)];
            let p = v.as_mut_ptr();
            core::mem::forget(v);
            p.cast()
        }
        unsafe extern "C" fn zfree(_opaque: *mut c_void, address: *mut c_void) {
            FREES.fetch_add(1, Ordering::SeqCst);
            // We cannot recover the exact length here, so this test backing
            // store intentionally leaks the bytes (the process ends promptly);
            // the assertion of interest is that `zfree` is invoked exactly once
            // per `zalloc`. This keeps the test allocator trivially sound.
            let _ = address;
        }

        let alloc = CAllocator {
            zalloc: Some(zalloc),
            zfree: Some(zfree),
            opaque: ptr::null_mut(),
        };
        assert!(alloc.hook().is_active());

        {
            let buf: AllocBuffer<u16> = alloc
                .allocate_zeroed(32)
                .expect("active-hook zalloc returns non-null");
            assert_eq!(buf.len(), 32);
            // Foreign region is zero-filled and usable as a slice.
            assert!(buf.iter().all(|&w| w == 0));
        } // <- drop routes through the caller's `zfree`

        assert_eq!(ALLOCS.load(Ordering::SeqCst), 1, "zalloc must be invoked");
        assert_eq!(FREES.load(Ordering::SeqCst), 1, "zfree must balance zalloc");
    }

    /// M7 regression: an **active** allocator hook whose `zalloc` reports
    /// out-of-memory (returns `NULL`) must make `allocate_zeroed` yield
    /// [`None`] — surfaced as `Z_MEM_ERROR` at the engine init paths — rather
    /// than silently falling back to the Rust global allocator. A zero-count
    /// request still succeeds through the owned fast path (the hook is never
    /// consulted).
    #[test]
    fn callocator_active_hook_oom_returns_none_no_global_fallback() {
        unsafe extern "C" fn oom_zalloc(
            _opaque: *mut c_void,
            _items: c_uint,
            _size: c_uint,
        ) -> *mut c_void {
            // Report out-of-memory unconditionally.
            ptr::null_mut()
        }
        unsafe extern "C" fn noop_zfree(_opaque: *mut c_void, _address: *mut c_void) {}

        let alloc = CAllocator {
            zalloc: Some(oom_zalloc),
            zfree: Some(noop_zfree),
            opaque: ptr::null_mut(),
        };
        assert!(alloc.hook().is_active());

        // A non-empty request through the OOM hook must fail with `None`; there
        // is deliberately NO global-allocator fallback (M7).
        let buf: Option<AllocBuffer<u8>> = alloc.allocate_zeroed(64);
        assert!(
            buf.is_none(),
            "active-hook OOM must yield None (M7), not a global-allocator Vec"
        );

        // The zero-count fast path never consults the hook and still succeeds.
        let empty: AllocBuffer<u8> = alloc
            .allocate_zeroed(0)
            .expect("empty request uses the owned fast path");
        assert!(empty.is_empty());
    }

    /// M7 propagation: a real engine init (`DeflateState::new_in`) driven by an
    /// active OOM hook surfaces [`ZlibError::MemError`] (→ `Z_MEM_ERROR`) instead
    /// of panicking or silently succeeding on the global allocator. This proves
    /// the `AllocBuffer::try_zeroed(..).ok_or(ZlibError::MemError)?` chain in
    /// `new_in` is wired end to end.
    #[test]
    fn deflate_new_in_surfaces_mem_error_on_active_hook_oom() {
        use crate::constants::{Strategy, Z_DEFLATED};
        use crate::deflate::state::DeflateState;
        use crate::error::ZlibError;

        unsafe extern "C" fn oom_zalloc(
            _opaque: *mut c_void,
            _items: c_uint,
            _size: c_uint,
        ) -> *mut c_void {
            ptr::null_mut()
        }
        unsafe extern "C" fn noop_zfree(_opaque: *mut c_void, _address: *mut c_void) {}

        // Build the hook the way the production FFI path does — through
        // `CAllocator::hook()` — so it carries the `ffi`-installed
        // `FOREIGN_VTABLE`. This exercises the real dependency-inverted
        // allocation path end to end: the active `zalloc` returning null is the
        // ONLY reason the init can fail.
        let alloc = CAllocator {
            zalloc: Some(oom_zalloc),
            zfree: Some(noop_zfree),
            opaque: ptr::null_mut(),
        };
        let hook = alloc.hook();
        assert!(hook.is_active());

        // Valid parameters (level 6, deflate method, 15-bit window, mem level 8,
        // default strategy, zlib wrap): the ONLY reason this can fail is the OOM
        // hook, so a `MemError` proves the M7 propagation path.
        let result = DeflateState::new_in(hook, 6, Z_DEFLATED, 15, 8, Strategy::Default, 1);
        assert!(
            matches!(result, Err(ZlibError::MemError)),
            "active-hook OOM at init must surface Z_MEM_ERROR (M7), got {:?}",
            result.as_ref().map(|_| "Ok(state)")
        );
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
