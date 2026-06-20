//! The idiomatic, ownership-enforcing stream abstraction (`ZStream`).
//!
//! This module defines [`ZStream`], the safe-Rust counterpart of the C
//! `z_stream` structure (`zlib.h`) — the central data carrier threaded through
//! every `deflate` and `inflate` call. It replaces the C structure's raw
//! pointers and manual `ZALLOC`/`ZFREE` lifecycle with Rust ownership: the
//! internal compression/decompression state is held as an owned
//! [`Box`]`<dyn `[`StreamState`]`>`, working buffers live inside that state as
//! owned `Vec`/`Box`, and teardown is automatic via [`Drop`] (replacing
//! `deflateEnd` / `inflateEnd`). See the [memory-ownership model](#memory-ownership-model)
//! below and the AAP §0.6.3 design it implements.
//!
//! # Relationship to the C `z_stream`
//!
//! The C structure (`zlib.h`, `struct z_stream_s`) carries **fourteen** fields
//! in a fixed ABI order:
//!
//! ```text
//! z_const Bytef *next_in;   uInt  avail_in;   uLong total_in;
//! Bytef         *next_out;  uInt  avail_out;  uLong total_out;
//! z_const char  *msg;       struct internal_state *state;
//! alloc_func     zalloc;    free_func zfree;  voidpf opaque;
//! int            data_type; uLong adler;      uLong reserved;
//! ```
//!
//! `ZStream` is the idiomatic *twin* of that structure. The **`#[repr(C)]`
//! `z_stream`** with all fourteen raw fields — the actual ABI struct that the
//! cbindgen-generated C header exposes — lives in [`crate::ffi`], not here.
//! This module documents the field-by-field correspondence so the FFI layer
//! can marshal between the two:
//!
//! | C field(s)                         | C type            | `ZStream` handling                                   |
//! |------------------------------------|-------------------|------------------------------------------------------|
//! | `next_in`                          | `z_const Bytef *` | *not stored* — passed per call as `&[u8]` (see below) |
//! | `avail_in`                         | `uInt`            | [`avail_in`](ZStream::avail_in): `u32` (private, getter) |
//! | `total_in`                         | `uLong`           | [`total_in`](ZStream::total_in): `u64`               |
//! | `next_out`                         | `Bytef *`         | *not stored* — passed per call as `&mut [u8]`        |
//! | `avail_out`                        | `uInt`            | [`avail_out`](ZStream::avail_out): `u32` (private, getter) |
//! | `total_out`                        | `uLong`           | [`total_out`](ZStream::total_out): `u64`             |
//! | `msg`                              | `z_const char *`  | [`msg`](ZStream::msg): `Option<&'static str>`        |
//! | `state`                            | `internal_state *`| `Option<Box<dyn StreamState>>` (private, owned)      |
//! | `zalloc` + `zfree` + `opaque`      | func ptrs + `void*`| `allocator: A` ([`Allocator`] trait)                |
//! | `data_type`                        | `int`             | [`data_type`](ZStream::data_type): `i32`             |
//! | `adler`                            | `uLong`           | [`adler`](ZStream::adler): `u32`                     |
//! | `reserved`                         | `uLong`           | *no counterpart* (exists only in `ffi`'s `z_stream`) |
//!
//! ## The lifetime-free design
//!
//! In C, `next_in`/`avail_in` and `next_out`/`avail_out` are a pointer-plus-count
//! pair that the caller refreshes before each `deflate`/`inflate` call. The
//! idiomatic engine functions instead take the input and output buffers **per
//! call** as `&[u8]` / `&mut [u8]` slices:
//!
//! ```ignore
//! // (illustrative engine signature implemented in `crate::deflate`)
//! fn deflate(strm: &mut ZStream, input: &[u8], output: &mut [u8], flush: FlushMode)
//!     -> Result<ReturnCode>;
//! ```
//!
//! Consequently `ZStream` **never stores borrowed slices** — it is entirely
//! lifetime-free (`ZStream<A>`, no `'a`). It owns only *accounting* (the
//! cumulative [`total_in`](ZStream::total_in) / [`total_out`](ZStream::total_out)
//! counters and the most-recent [`avail_in`](ZStream::avail_in) /
//! [`avail_out`](ZStream::avail_out) bookkeeping) plus the boxed internal state.
//! This is what lets a single `ZStream` be a long-lived owner threaded through
//! many calls without fighting the borrow checker, mirroring C's per-call
//! `next_in`/`next_out` refresh. The FFI layer keeps the `#[repr(C)] z_stream`'s
//! `avail_in`/`avail_out` in correspondence with the values reported here.
//!
//! # Memory-ownership model
//!
//! `ZStream` realises the AAP §0.6.3 ownership tree:
//!
//! ```text
//! ZStream<A>
//!  ├─ owns  Option<Box<dyn StreamState>>   (Some after init, None before / after end)
//!  │         └─ the concrete DeflateState / InflateState owns its working
//!  │            buffers: window: Vec<u8>, pending_buf: Vec<u8>, head/prev:
//!  │            Box<[u16]>, codes: Box<[Code]>, …
//!  └─ holds  A: Allocator                  (the zalloc/zfree/opaque triple)
//! ```
//!
//! Dropping a `ZStream` drops the `Box<dyn StreamState>`, which in turn drops
//! every owned buffer — a deterministic, leak-free teardown that **replaces**
//! `deflateEnd` / `inflateEnd` / `gzclose`. [`ZStream::end`] makes this explicit
//! by simply taking `self.state = None`. Double-free and use-after-free are
//! *unrepresentable* here because ownership is enforced by the borrow checker.
//!
//! ## Breaking the dependency cycle
//!
//! The concrete state types live in `crate::deflate` and `crate::inflate`, both
//! of which depend on this module. To avoid a circular dependency, `ZStream`
//! owns a [`StreamState`] **trait object** rather than importing the engine
//! types: this module declares the minimal object-safe [`StreamState`]
//! interface, and the engines implement it for their concrete `DeflateState` /
//! `InflateState`. Thus dependencies flow strictly *downward*
//! (`deflate`/`inflate` → `stream`), never upward.
//!
//! # No `unsafe`
//!
//! This module contains **no `unsafe` blocks**. The custom-allocator path that
//! dereferences the C `zalloc`/`zfree` function pointers, and the `#[repr(C)]`
//! pointer marshalling, are confined to [`crate::ffi`] — one of the two
//! sanctioned `unsafe` sites in the crate alongside the `inflate::fast` inner
//! loop (AAP §0.6.2). The [`Allocator`] trait is a *safe* abstraction over
//! those hooks: the default [`GlobalAlloc`] backs allocations with owned
//! `Vec`/`Box`, and a custom allocator that bridges raw C pointers is supplied
//! by `crate::ffi` (which performs the unsafe pointer work internally).
//!
//! # `no_std`
//!
//! This module is `no_std`-clean. It needs the [`alloc`] crate for [`Box`]
//! (the owned state) and [`Vec`] (the [`GlobalAlloc`] backing buffer); under the
//! crate's `no-std` feature those come from [`alloc`] (via the crate-level
//! `extern crate alloc;` in `lib.rs`), and under the default `std` build they
//! come from the prelude. No `std`-only items are used. Any `std::io`
//! `Read`/`Write` streaming wrappers live in the `gz` layer, not here.
//!
//! # Examples
//!
//! ```
//! use zlib_rs::stream::ZStream;
//!
//! // A freshly constructed stream is an empty, uninitialised carrier — the
//! // idiomatic equivalent of a zero-initialised C `z_stream`.
//! let mut strm = ZStream::new();
//! assert_eq!(strm.total_in, 0);
//! assert_eq!(strm.total_out, 0);
//! assert_eq!(strm.adler, 0);
//! assert!(!strm.has_state());
//!
//! // `end()` deterministically tears down any owned internal state, replacing
//! // the C `deflateEnd` / `inflateEnd` call (here it is a no-op since no state
//! // has been installed yet).
//! strm.end();
//! assert!(!strm.has_state());
//! ```

#[cfg(not(feature = "std"))]
use alloc::{boxed::Box, vec::Vec};

use core::fmt;
use core::ptr::NonNull;

use crate::constants::{DataType, Z_BINARY};
use crate::error::ZlibError;

// ===========================================================================
// Allocator — the safe abstraction over the C `zalloc`/`zfree`/`opaque` hooks.
// ===========================================================================

/// Abstraction over the C `zalloc` / `zfree` / `opaque` allocator triple.
///
/// C `zlib` lets a caller plug in a custom allocator through three fields of
/// `z_stream`:
///
/// ```text
/// typedef voidpf (*alloc_func)(voidpf opaque, uInt items, uInt size);
/// typedef void   (*free_func)(voidpf opaque, voidpf address);
/// ```
///
/// `Allocator` preserves those semantics idiomatically (AAP §0.6.3): an
/// implementor carries the equivalent of `opaque` as its own state, and the
/// two methods mirror `zalloc(opaque, items, size)` and `zfree(opaque, addr)`.
///
/// # A *safe* trait
///
/// This is a **safe** trait (not an `unsafe trait`), and both methods are
/// **safe** to call. That is a deliberate design choice (AAP §0.6.2): the
/// pure-Rust compression core never allocates through this trait at all — its
/// working buffers are owned `Vec`/`Box` inside the engine state, reclaimed by
/// RAII — so keeping the methods safe means this module, and the safe core,
/// remain entirely free of `unsafe`. The only implementor that performs raw
/// pointer work is the custom allocator in [`crate::ffi`], which wraps the C
/// `zalloc`/`zfree` function pointers and confines its `unsafe` blocks to that
/// boundary.
///
/// # Contract
///
/// Implementors must honour the following so that callers (chiefly
/// [`crate::ffi`]) can rely on the abstraction:
///
/// * [`allocate`](Allocator::allocate) returns either a non-null pointer to a
///   block of at least `count * size` bytes, or `None` if the request cannot be
///   satisfied (out of memory) or `count * size` overflows / is zero.
/// * [`deallocate`](Allocator::deallocate) must be passed a pointer previously
///   returned by **this same allocator's** [`allocate`](Allocator::allocate),
///   together with the *same* `count` and `size`, and that pointer must not
///   have been freed already. Violating this is a logic error; because the
///   real raw-pointer implementor lives in [`crate::ffi`], the unsafety of the
///   underlying free is encapsulated there rather than exposed in this
///   signature.
///
/// The default implementation, [`GlobalAlloc`], backs allocations with the Rust
/// global allocator and is the default type parameter of [`ZStream`].
pub trait Allocator {
    /// Allocate a block of at least `count * size` bytes.
    ///
    /// Mirrors C `zalloc(opaque, items, size)`. Returns `None` when the
    /// allocation fails, or when `count * size` is zero or overflows `usize`.
    /// The returned memory may be uninitialised or zeroed depending on the
    /// implementor; [`GlobalAlloc`] returns zeroed memory.
    fn allocate(&self, count: usize, size: usize) -> Option<NonNull<u8>>;

    /// Release a block previously obtained from [`allocate`](Allocator::allocate).
    ///
    /// Mirrors C `zfree(opaque, address)`. See the [trait contract](Allocator#contract)
    /// for the requirements `ptr`, `count`, and `size` must satisfy.
    fn deallocate(&self, ptr: NonNull<u8>, count: usize, size: usize);
}

/// The default [`Allocator`]: the Rust global allocator.
///
/// `GlobalAlloc` is a zero-sized marker selecting Rust's global heap. It is the
/// default type parameter of [`ZStream`], so pure-Rust callers simply write
/// [`ZStream::new`] and never think about allocation.
///
/// # Allocation strategy (no `unsafe`)
///
/// [`allocate`](GlobalAlloc::allocate) obtains a zeroed block by building an
/// owned `Box<[u8]>` and handing out a raw pointer to it via [`Box::into_raw`]
/// — both `Box::into_raw` and [`NonNull::new`] are safe, so this file needs no
/// `unsafe` block. Reclaiming such a raw block requires [`Box::from_raw`], an
/// `unsafe` operation; per AAP §0.6.2 that operation is confined to
/// [`crate::ffi`]. Accordingly [`deallocate`](GlobalAlloc::deallocate) here is a
/// **documented no-op**:
///
/// * The pure-Rust core never routes buffer allocation through this allocator
///   (its buffers are owned `Vec`/`Box`, freed by `Drop` — RAII, AAP §0.6.3),
///   so in the default path neither method is ever called and nothing leaks.
/// * When the FFI layer *does* obtain a raw block from
///   [`allocate`](GlobalAlloc::allocate) (e.g. to back a C caller that supplied
///   no custom allocator), it reclaims that block itself with the `unsafe`
///   `Box::from_raw` in [`crate::ffi`], rather than through this no-op.
///
/// # Examples
///
/// ```
/// use zlib_rs::stream::{Allocator, GlobalAlloc};
///
/// let a = GlobalAlloc;
/// // A zero-sized request yields `None` (mirrors "nothing to allocate").
/// assert!(a.allocate(0, 8).is_none());
/// assert!(a.allocate(8, 0).is_none());
/// ```
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct GlobalAlloc;

impl Allocator for GlobalAlloc {
    #[inline]
    fn allocate(&self, count: usize, size: usize) -> Option<NonNull<u8>> {
        // Total size with overflow protection; a zero request allocates nothing.
        let total = count.checked_mul(size)?;
        if total == 0 {
            return None;
        }

        // Build a zeroed, owned boxed slice using only safe APIs, then leak it
        // to a raw pointer. `try_reserve_exact` makes the allocation fallible
        // (returning `None` on OOM instead of panicking), matching the C
        // `zalloc` "returns NULL on failure" contract. No `unsafe` is required:
        // `Box::into_raw` and `NonNull::new` are both safe.
        let mut buffer: Vec<u8> = Vec::new();
        buffer.try_reserve_exact(total).ok()?;
        buffer.resize(total, 0u8);
        let raw = Box::into_raw(buffer.into_boxed_slice()) as *mut u8;
        NonNull::new(raw)
    }

    #[inline]
    fn deallocate(&self, _ptr: NonNull<u8>, _count: usize, _size: usize) {
        // Intentional no-op — see the type-level documentation. The pure-Rust
        // core reclaims all memory through `Drop` (RAII) and never calls this;
        // any raw block vended by `allocate` is reclaimed by the `unsafe`
        // `Box::from_raw` in `crate::ffi`, the sanctioned unsafe site
        // (AAP §0.6.2). Performing the free here would require an `unsafe`
        // block, which this module forbids.
    }
}

// ===========================================================================
// StreamState — the object-safe interface for the owned internal state.
// ===========================================================================

/// Which engine a [`StreamState`] belongs to.
///
/// Returned by [`StreamState::kind`], this lets the stream and the FFI layer
/// distinguish a deflate (compression) state from an inflate (decompression)
/// state without downcasting or importing the concrete engine types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StreamKind {
    /// A compression (`deflate`) state.
    Deflate,
    /// A decompression (`inflate`) state.
    Inflate,
}

/// Object-safe interface for the boxed internal compression/decompression state.
///
/// [`ZStream`] owns its engine state as an `Option<Box<dyn StreamState>>`. This
/// trait is the seam that **breaks the dependency cycle** between this module
/// and the engines: the concrete `DeflateState` (`crate::deflate::state`) and
/// `InflateState` (`crate::inflate::state`) implement `StreamState`, so this
/// module need not — and must not — import them. Dependencies therefore flow
/// only downward (`deflate`/`inflate` → `stream`), never the reverse.
///
/// # Object safety
///
/// Every method takes `&self` or `&mut self` and uses no generic parameters or
/// `Self`-by-value, so `dyn StreamState` is a valid trait object. The interface
/// is intentionally **minimal**: it exposes only what the stream layer needs to
/// identify and reset the state. All engine-specific behaviour (the actual
/// `deflate`/`inflate` algorithms, buffer access, parameter tuning) lives on the
/// concrete types, reached by the engine modules that own them.
///
/// # Teardown
///
/// No teardown method is required: because the state is held in a [`Box`],
/// dropping the [`ZStream`] (or calling [`ZStream::end`]) drops the box, which
/// runs the concrete type's own [`Drop`] and frees every owned buffer. This is
/// the RAII replacement for `deflateEnd` / `inflateEnd` (AAP §0.6.3).
pub trait StreamState {
    /// Identify whether this is a deflate or inflate state.
    fn kind(&self) -> StreamKind;

    /// Reset the internal state to its post-initialisation condition.
    ///
    /// Mirrors the engine-internal portion of `deflateReset` / `inflateReset`:
    /// counters, bit buffers, and Huffman/window bookkeeping return to their
    /// freshly-initialised values while the already-allocated buffers are
    /// retained (no reallocation). The stream-level accounting that lives on
    /// [`ZStream`] (totals, message) is reset separately by [`ZStream::reset`].
    fn reset(&mut self);

    /// Convenience predicate: is this a [`StreamKind::Deflate`] state?
    ///
    /// Provided method; implementors normally need not override it.
    #[inline]
    fn is_deflate(&self) -> bool {
        matches!(self.kind(), StreamKind::Deflate)
    }

    /// Convenience predicate: is this a [`StreamKind::Inflate`] state?
    ///
    /// Provided method; implementors normally need not override it.
    #[inline]
    fn is_inflate(&self) -> bool {
        matches!(self.kind(), StreamKind::Inflate)
    }
}

// ===========================================================================
// ZStream — the idiomatic stream carrier.
// ===========================================================================

/// The idiomatic, ownership-enforcing compression/decompression stream.
///
/// `ZStream` is the safe-Rust twin of the C `z_stream` (`zlib.h`). It carries
/// the user-facing accounting fields, the last error message, the best-guess
/// data type, the running checksum, and — owned behind a [`Box`] — the engine's
/// internal [`StreamState`]. The allocator is abstracted by the type parameter
/// `A`, which defaults to [`GlobalAlloc`] so pure-Rust callers can simply write
/// `ZStream::new()`.
///
/// See the [module documentation](self) for the full field-by-field
/// correspondence with the C structure, the lifetime-free design rationale, and
/// the memory-ownership model.
///
/// # Lifetime-free
///
/// `ZStream` stores **no borrowed input/output slices**: the per-call `next_in`
/// / `next_out` buffers are passed to the engine functions as `&[u8]` /
/// `&mut [u8]` arguments instead. The struct therefore has no lifetime
/// parameter and can be owned for the entire duration of a stream, threaded
/// through arbitrarily many calls.
///
/// # Examples
///
/// ```
/// use zlib_rs::stream::ZStream;
///
/// let mut strm = ZStream::new();
/// assert_eq!(strm.total_in, 0);
/// assert!(strm.msg.is_none());
/// assert!(!strm.has_state());
/// ```
pub struct ZStream<A: Allocator = GlobalAlloc> {
    /// Total number of input bytes consumed so far (C `uLong total_in`).
    ///
    /// Widened to [`u64`] for 64-bit-clean accounting; the value the FFI layer
    /// reports as the C `uLong total_in` is this counter. Updated by the engine
    /// via [`add_total_in`](ZStream::add_total_in), which wraps on overflow to
    /// match C's unsigned semantics.
    pub total_in: u64,

    /// Total number of output bytes produced so far (C `uLong total_out`).
    ///
    /// Widened to [`u64`] exactly as [`total_in`](ZStream::total_in). Updated by
    /// the engine via [`add_total_out`](ZStream::add_total_out).
    pub total_out: u64,

    /// Last error message, or `None` if no error (C `z_const char *msg`).
    ///
    /// In C, `msg` points into the static `z_errmsg` string table, so the
    /// idiomatic form is a `&'static str`: the messages come from
    /// [`ZlibError::message`] (see [`set_msg_from_err`](ZStream::set_msg_from_err)),
    /// which returns `&'static str`. No heap allocation is involved, keeping the
    /// field `no_std`-friendly.
    pub msg: Option<&'static str>,

    /// Best-guess data type (C `int data_type`).
    ///
    /// For deflate this is the binary/text heuristic ([`crate::constants::Z_BINARY`]
    /// / `Z_TEXT` / `Z_UNKNOWN`); for inflate it carries decode-state bits. Kept
    /// as a raw [`i32`] to mirror the C `int`; use
    /// [`data_type_hint`](ZStream::data_type_hint) for a typed
    /// [`DataType`] view of the binary/text/unknown cases.
    pub data_type: i32,

    /// Running checksum of the *uncompressed* data (C `uLong adler`).
    ///
    /// Holds either an Adler-32 (zlib wrapper) or a CRC-32 (gzip wrapper) value.
    /// Although C declares it `uLong`, the value is always 32-bit, so this is a
    /// [`u32`]. A freshly constructed stream has `adler == 0`; the engine's
    /// initialisation seeds it with the checksum's identity (`1` for Adler-32,
    /// `0` for CRC-32) for the selected wrapper.
    pub adler: u32,

    /// Bytes available at the current input position as of the last engine call
    /// (C `uInt avail_in`). Private bookkeeping exposed by
    /// [`avail_in`](ZStream::avail_in); see the lifetime-free design note in the
    /// [module docs](self).
    avail_in: u32,

    /// Free space remaining at the current output position as of the last engine
    /// call (C `uInt avail_out`). Private bookkeeping exposed by
    /// [`avail_out`](ZStream::avail_out).
    avail_out: u32,

    /// The owned internal engine state (C `internal_state *state`).
    ///
    /// `None` before initialisation and after [`end`](ZStream::end); `Some` once
    /// an engine installs its [`StreamState`]. Held in a [`Box`] so that dropping
    /// the stream frees the state and all of its owned buffers (RAII).
    state: Option<Box<dyn StreamState>>,

    /// The allocator abstraction (C `zalloc` + `zfree` + `opaque`).
    ///
    /// Accessed via [`allocator`](ZStream::allocator). Defaults to
    /// [`GlobalAlloc`]; custom allocators (e.g. the FFI bridge to C
    /// `zalloc`/`zfree`) are supplied through
    /// [`with_allocator`](ZStream::with_allocator).
    allocator: A,
}

impl ZStream<GlobalAlloc> {
    /// Create a new, uninitialised stream backed by the global allocator.
    ///
    /// The returned stream is an empty carrier — the idiomatic equivalent of a
    /// zero-initialised C `z_stream`: all counters are `0`, [`msg`](ZStream::msg)
    /// is `None`, [`data_type`](ZStream::data_type) is
    /// [`Z_BINARY`](crate::constants::Z_BINARY) (`0`), [`adler`](ZStream::adler)
    /// is `0`, and no [`StreamState`] is installed
    /// ([`has_state`](ZStream::has_state) is `false`). The deflate / inflate
    /// initialisation routines later install the engine state and seed
    /// [`adler`](ZStream::adler) (to `1` for Adler-32 or `0` for CRC-32) and
    /// [`data_type`](ZStream::data_type) (to `Z_UNKNOWN`) for the chosen
    /// wrapper.
    ///
    /// # Examples
    ///
    /// ```
    /// use zlib_rs::stream::ZStream;
    ///
    /// let strm = ZStream::new();
    /// assert_eq!(strm.total_in, 0);
    /// assert_eq!(strm.total_out, 0);
    /// assert_eq!(strm.adler, 0);
    /// assert!(strm.msg.is_none());
    /// assert!(!strm.has_state());
    /// ```
    #[inline]
    #[must_use]
    pub fn new() -> Self {
        Self::with_allocator(GlobalAlloc)
    }
}

impl Default for ZStream<GlobalAlloc> {
    /// Equivalent to [`ZStream::new`].
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

impl<A: Allocator> ZStream<A> {
    /// Create a new, uninitialised stream backed by a custom [`Allocator`].
    ///
    /// All fields are initialised exactly as in [`ZStream::new`]; only the
    /// allocator differs. This is the constructor the FFI layer uses when a C
    /// caller supplies its own `zalloc`/`zfree`/`opaque` triple (wrapped as an
    /// `Allocator` implementor in [`crate::ffi`]).
    ///
    /// # Examples
    ///
    /// ```
    /// use zlib_rs::stream::{GlobalAlloc, ZStream};
    ///
    /// // Explicitly selecting the default allocator is equivalent to `new()`.
    /// let strm = ZStream::with_allocator(GlobalAlloc);
    /// assert!(!strm.has_state());
    /// ```
    #[inline]
    #[must_use]
    pub fn with_allocator(allocator: A) -> Self {
        Self {
            total_in: 0,
            total_out: 0,
            msg: None,
            // A zeroed carrier matches a fresh C `z_stream` (`Z_BINARY == 0`);
            // engine init later sets `Z_UNKNOWN`.
            data_type: Z_BINARY,
            adler: 0,
            avail_in: 0,
            avail_out: 0,
            state: None,
            allocator,
        }
    }

    // -----------------------------------------------------------------------
    // I/O accounting accessors.
    // -----------------------------------------------------------------------

    /// Bytes available at the input position as of the last engine call (the C
    /// `avail_in`).
    ///
    /// In the lifetime-free design the per-call input is a `&[u8]` slice passed
    /// to the engine; this getter reports the bookkeeping value the engine left
    /// behind (typically the count of input bytes not yet consumed). The FFI
    /// layer keeps the `#[repr(C)] z_stream`'s `avail_in` in step with it.
    #[inline]
    #[must_use]
    pub fn avail_in(&self) -> u32 {
        self.avail_in
    }

    /// Free space remaining at the output position as of the last engine call
    /// (the C `avail_out`). See [`avail_in`](ZStream::avail_in).
    #[inline]
    #[must_use]
    pub fn avail_out(&self) -> u32 {
        self.avail_out
    }

    /// Record the input-available count (engine bookkeeping; mirrors writing the
    /// C `avail_in`).
    #[inline]
    pub fn set_avail_in(&mut self, avail_in: u32) {
        self.avail_in = avail_in;
    }

    /// Record the output-free count (engine bookkeeping; mirrors writing the C
    /// `avail_out`).
    #[inline]
    pub fn set_avail_out(&mut self, avail_out: u32) {
        self.avail_out = avail_out;
    }

    /// Add `n` to [`total_in`](ZStream::total_in), wrapping on overflow.
    ///
    /// The wrap matches C, where `total_in` is an unsigned `uLong` and the
    /// `strm->total_in += len` increment wraps silently. In practice the 64-bit
    /// counter never overflows for real workloads.
    #[inline]
    pub fn add_total_in(&mut self, n: u64) {
        self.total_in = self.total_in.wrapping_add(n);
    }

    /// Add `n` to [`total_out`](ZStream::total_out), wrapping on overflow. See
    /// [`add_total_in`](ZStream::add_total_in).
    #[inline]
    pub fn add_total_out(&mut self, n: u64) {
        self.total_out = self.total_out.wrapping_add(n);
    }

    /// Borrow the allocator backing this stream (the C `zalloc`/`zfree`/`opaque`
    /// triple, abstracted).
    #[inline]
    #[must_use]
    pub fn allocator(&self) -> &A {
        &self.allocator
    }

    // -----------------------------------------------------------------------
    // `data_type` accessors.
    // -----------------------------------------------------------------------

    /// Set [`data_type`](ZStream::data_type) from a typed [`DataType`].
    ///
    /// Convenience for the deflate path, whose data type is exactly one of
    /// [`DataType::Binary`] / [`DataType::Text`] / [`DataType::Unknown`]. The
    /// inflate path, which packs decode-state bits into `data_type`, should use
    /// [`set_data_type_raw`](ZStream::set_data_type_raw) instead.
    #[inline]
    pub fn set_data_type(&mut self, data_type: DataType) {
        self.data_type = i32::from(data_type);
    }

    /// Set [`data_type`](ZStream::data_type) from a raw C `int`.
    ///
    /// Use this on the inflate path, where `data_type` carries packed
    /// decode-state bits (e.g. last-block / header flags) that are not
    /// representable by the [`DataType`] enum.
    #[inline]
    pub fn set_data_type_raw(&mut self, data_type: i32) {
        self.data_type = data_type;
    }

    /// Interpret [`data_type`](ZStream::data_type) as a typed [`DataType`], if it
    /// is one of the recognised binary/text/unknown values.
    ///
    /// Returns `None` for any other bit pattern (such as the inflate decode-state
    /// values), mirroring [`DataType::try_from`].
    ///
    /// # Examples
    ///
    /// ```
    /// use zlib_rs::stream::ZStream;
    /// use zlib_rs::constants::DataType;
    ///
    /// let mut strm = ZStream::new();
    /// strm.set_data_type(DataType::Text);
    /// assert_eq!(strm.data_type_hint(), Some(DataType::Text));
    ///
    /// strm.set_data_type_raw(64); // an inflate decode-state value
    /// assert_eq!(strm.data_type_hint(), None);
    /// ```
    #[inline]
    #[must_use]
    pub fn data_type_hint(&self) -> Option<DataType> {
        DataType::try_from(self.data_type).ok()
    }

    // -----------------------------------------------------------------------
    // `msg` accessors.
    // -----------------------------------------------------------------------

    /// Set [`msg`](ZStream::msg) to the static message for `err`.
    ///
    /// Uses [`ZlibError::message`], which returns the exact C `z_errmsg` string
    /// for the error (a `&'static str`). This is the idiomatic counterpart of
    /// the C `ERR_MSG` / `strm->msg = (char *)ERR_MSG(err)` assignment.
    ///
    /// # Examples
    ///
    /// ```
    /// use zlib_rs::stream::ZStream;
    /// use zlib_rs::error::ZlibError;
    ///
    /// let mut strm = ZStream::new();
    /// strm.set_msg_from_err(ZlibError::DataError);
    /// assert_eq!(strm.msg, Some("data error"));
    /// ```
    #[inline]
    pub fn set_msg_from_err(&mut self, err: ZlibError) {
        self.msg = Some(err.message());
    }

    /// Set [`msg`](ZStream::msg) to an explicit static string.
    #[inline]
    pub fn set_msg(&mut self, msg: &'static str) {
        self.msg = Some(msg);
    }

    /// Clear [`msg`](ZStream::msg) back to `None` (no error).
    #[inline]
    pub fn clear_msg(&mut self) {
        self.msg = None;
    }

    // -----------------------------------------------------------------------
    // Internal-state ownership and lifecycle.
    // -----------------------------------------------------------------------

    /// Borrow the installed [`StreamState`], if any.
    #[inline]
    #[must_use]
    pub fn state(&self) -> Option<&dyn StreamState> {
        self.state.as_deref()
    }

    /// Mutably borrow the installed [`StreamState`], if any.
    ///
    /// The returned trait object carries a `'static` bound because the state is
    /// owned outright (held in a `Box<dyn StreamState>`, which is itself
    /// `'static`-bounded); the mutable borrow is tied to `&mut self`. The
    /// explicit `'static` is required because mutable references are invariant
    /// over their referent (unlike the covariant shared borrow returned by
    /// [`state`](ZStream::state)).
    #[inline]
    pub fn state_mut(&mut self) -> Option<&mut (dyn StreamState + 'static)> {
        self.state.as_deref_mut()
    }

    /// Install the engine's internal [`StreamState`], taking ownership.
    ///
    /// Called by `deflateInit` / `inflateInit` after they build the concrete
    /// `DeflateState` / `InflateState`. Any previously installed state is
    /// dropped (freeing its buffers).
    #[inline]
    pub fn set_state(&mut self, state: Box<dyn StreamState>) {
        self.state = Some(state);
    }

    /// Take ownership of the installed [`StreamState`], leaving `None` behind.
    ///
    /// Useful for engine code that needs to move the state out (for example to
    /// re-wrap or replace it). After this call [`has_state`](ZStream::has_state)
    /// is `false`.
    #[inline]
    pub fn take_state(&mut self) -> Option<Box<dyn StreamState>> {
        self.state.take()
    }

    /// Returns `true` if an internal [`StreamState`] is currently installed
    /// (i.e. the stream has been initialised and not yet ended).
    #[inline]
    #[must_use]
    pub fn has_state(&self) -> bool {
        self.state.is_some()
    }

    /// The [`StreamKind`] of the installed state, or `None` if uninitialised.
    #[inline]
    #[must_use]
    pub fn kind(&self) -> Option<StreamKind> {
        self.state.as_ref().map(|s| s.kind())
    }

    /// Reset the stream-level accounting and the internal state.
    ///
    /// Mirrors the idiomatic portion of `deflateReset` / `inflateReset`: the
    /// cumulative totals and the available-byte bookkeeping are zeroed, the
    /// error [`msg`](ZStream::msg) is cleared, and the installed
    /// [`StreamState`], if any, is reset in place via [`StreamState::reset`]
    /// (its already-allocated buffers are retained — no reallocation).
    ///
    /// The wrapper-specific [`adler`](ZStream::adler) seed and
    /// [`data_type`](ZStream::data_type) are re-established by the engine's own
    /// reset/init (which knows whether the stream is zlib, raw, or gzip) and are
    /// intentionally left untouched here, since this lifetime-free twin does not
    /// track the active wrapper.
    ///
    /// # Examples
    ///
    /// ```
    /// use zlib_rs::stream::ZStream;
    ///
    /// let mut strm = ZStream::new();
    /// strm.add_total_in(100);
    /// strm.add_total_out(40);
    /// strm.set_msg("data error");
    ///
    /// strm.reset();
    /// assert_eq!(strm.total_in, 0);
    /// assert_eq!(strm.total_out, 0);
    /// assert!(strm.msg.is_none());
    /// ```
    pub fn reset(&mut self) {
        self.total_in = 0;
        self.total_out = 0;
        self.avail_in = 0;
        self.avail_out = 0;
        self.msg = None;
        if let Some(state) = self.state.as_mut() {
            state.reset();
        }
    }

    /// End the stream: drop the internal state and free all of its buffers.
    ///
    /// Sets `self.state = None`, dropping the `Box<dyn StreamState>`. That drop
    /// runs the concrete state's [`Drop`], releasing its window, pending buffer,
    /// Huffman tables, and so on — a deterministic, leak-free teardown that
    /// **replaces** the C `deflateEnd` / `inflateEnd`. After this call
    /// [`has_state`](ZStream::has_state) is `false`; the accounting fields are
    /// left as-is (matching C, which does not zero the totals on `*End`).
    ///
    /// Calling `end` on an already-ended stream is harmless (a no-op).
    ///
    /// # Examples
    ///
    /// ```
    /// use zlib_rs::stream::ZStream;
    ///
    /// let mut strm = ZStream::new();
    /// strm.end();
    /// assert!(!strm.has_state());
    /// ```
    #[inline]
    pub fn end(&mut self) {
        self.state = None;
    }
}

impl<A: Allocator> fmt::Debug for ZStream<A> {
    /// Renders the accounting fields, the error message, the data type, the
    /// checksum, and the [`StreamKind`] of the installed state (if any).
    ///
    /// The allocator and the concrete state contents are deliberately omitted,
    /// so the impl requires neither `A: Debug` nor `StreamState: Debug`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ZStream")
            .field("total_in", &self.total_in)
            .field("total_out", &self.total_out)
            .field("avail_in", &self.avail_in)
            .field("avail_out", &self.avail_out)
            .field("msg", &self.msg)
            .field("data_type", &self.data_type)
            .field("adler", &self.adler)
            .field("state", &self.state.as_ref().map(|s| s.kind()))
            .finish()
    }
}

// ===========================================================================
// Tests — verify the ownership model, the accessors, and the no-`unsafe`
// allocator/state abstractions against the C `z_stream` semantics.
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::rc::Rc;
    use core::cell::Cell;

    /// Bundle returned by [`drop_flag_state`]: the boxed mock state plus the
    /// shared "was dropped" flag and "reset count" cell the test inspects.
    /// Aliased to keep the helper's signature readable (clippy `type_complexity`).
    type MockStateParts = (Box<dyn StreamState>, Rc<Cell<bool>>, Rc<Cell<usize>>);

    // --- Test doubles --------------------------------------------------------

    /// A [`StreamState`] mock that flips a shared flag when dropped, so a test
    /// can observe that dropping/ending a [`ZStream`] runs the boxed state's
    /// [`Drop`] (the RAII replacement for `deflateEnd`/`inflateEnd`).
    struct DropFlagState {
        kind: StreamKind,
        dropped: Rc<Cell<bool>>,
        resets: Rc<Cell<usize>>,
    }

    impl StreamState for DropFlagState {
        fn kind(&self) -> StreamKind {
            self.kind
        }
        fn reset(&mut self) {
            self.resets.set(self.resets.get() + 1);
        }
    }

    impl Drop for DropFlagState {
        fn drop(&mut self) {
            self.dropped.set(true);
        }
    }

    fn drop_flag_state(kind: StreamKind) -> MockStateParts {
        let dropped = Rc::new(Cell::new(false));
        let resets = Rc::new(Cell::new(0usize));
        let state = Box::new(DropFlagState {
            kind,
            dropped: Rc::clone(&dropped),
            resets: Rc::clone(&resets),
        });
        (state, dropped, resets)
    }

    /// An [`Allocator`] mock that counts `allocate`/`deallocate` invocations.
    ///
    /// `allocate` vends a well-aligned dangling pointer that is never
    /// dereferenced (so the mock performs no real allocation and cannot leak),
    /// which is sufficient to verify that the methods are invoked — and proves
    /// the abstraction is exercisable with **zero `unsafe`** at the call site.
    struct CountingAlloc {
        allocs: Cell<usize>,
        deallocs: Cell<usize>,
    }

    impl CountingAlloc {
        fn new() -> Self {
            Self {
                allocs: Cell::new(0),
                deallocs: Cell::new(0),
            }
        }
    }

    impl Allocator for CountingAlloc {
        fn allocate(&self, count: usize, size: usize) -> Option<NonNull<u8>> {
            self.allocs.set(self.allocs.get() + 1);
            // Honour the zero/overflow contract like a real allocator.
            if count.checked_mul(size)? == 0 {
                return None;
            }
            Some(NonNull::dangling())
        }

        fn deallocate(&self, _ptr: NonNull<u8>, _count: usize, _size: usize) {
            self.deallocs.set(self.deallocs.get() + 1);
        }
    }

    // --- Construction / defaults --------------------------------------------

    #[test]
    fn new_is_a_zeroed_uninitialised_carrier() {
        let strm = ZStream::new();
        assert_eq!(strm.total_in, 0);
        assert_eq!(strm.total_out, 0);
        assert_eq!(strm.avail_in(), 0);
        assert_eq!(strm.avail_out(), 0);
        assert_eq!(strm.adler, 0);
        assert_eq!(strm.data_type, Z_BINARY);
        assert!(strm.msg.is_none());
        assert!(!strm.has_state());
        assert!(strm.state().is_none());
        assert_eq!(strm.kind(), None);
    }

    #[test]
    fn default_equals_new() {
        let a: ZStream = ZStream::default();
        let b = ZStream::new();
        // Compare observable fields (no `PartialEq` on the trait-object state).
        assert_eq!(a.total_in, b.total_in);
        assert_eq!(a.total_out, b.total_out);
        assert_eq!(a.adler, b.adler);
        assert_eq!(a.data_type, b.data_type);
        assert_eq!(a.has_state(), b.has_state());
    }

    #[test]
    fn default_type_parameter_is_global_alloc() {
        // `ZStream::new()` must resolve to `ZStream<GlobalAlloc>` via the default
        // type parameter; this assignment only compiles if that holds.
        let strm: ZStream<GlobalAlloc> = ZStream::new();
        let _: &GlobalAlloc = strm.allocator();
    }

    #[test]
    fn with_allocator_uses_the_supplied_allocator() {
        let strm = ZStream::with_allocator(CountingAlloc::new());
        assert!(!strm.has_state());
        // Allocator is reachable and has not been invoked yet.
        assert_eq!(strm.allocator().allocs.get(), 0);
        assert_eq!(strm.allocator().deallocs.get(), 0);
    }

    // --- Accounting accessors -----------------------------------------------

    #[test]
    fn totals_accumulate_with_wraparound() {
        let mut strm = ZStream::new();
        strm.add_total_in(10);
        strm.add_total_in(5);
        strm.add_total_out(7);
        assert_eq!(strm.total_in, 15);
        assert_eq!(strm.total_out, 7);

        // Wrap-around mirrors C's unsigned `uLong` increment.
        strm.total_in = u64::MAX;
        strm.add_total_in(1);
        assert_eq!(strm.total_in, 0);
    }

    #[test]
    fn avail_bookkeeping_roundtrips() {
        let mut strm = ZStream::new();
        strm.set_avail_in(1234);
        strm.set_avail_out(5678);
        assert_eq!(strm.avail_in(), 1234);
        assert_eq!(strm.avail_out(), 5678);
    }

    // --- `data_type` accessors ----------------------------------------------

    #[test]
    fn data_type_typed_and_raw_setters() {
        let mut strm = ZStream::new();

        strm.set_data_type(DataType::Text);
        assert_eq!(strm.data_type, i32::from(DataType::Text));
        assert_eq!(strm.data_type_hint(), Some(DataType::Text));

        strm.set_data_type(DataType::Binary);
        assert_eq!(strm.data_type_hint(), Some(DataType::Binary));

        // A raw inflate decode-state value is not a recognised `DataType`.
        strm.set_data_type_raw(64);
        assert_eq!(strm.data_type, 64);
        assert_eq!(strm.data_type_hint(), None);
    }

    // --- `msg` accessors -----------------------------------------------------

    #[test]
    fn msg_helpers_set_and_clear() {
        let mut strm = ZStream::new();
        assert!(strm.msg.is_none());

        strm.set_msg_from_err(ZlibError::DataError);
        assert_eq!(strm.msg, Some("data error"));

        strm.set_msg_from_err(ZlibError::MemError);
        assert_eq!(strm.msg, Some("insufficient memory"));

        strm.set_msg("custom");
        assert_eq!(strm.msg, Some("custom"));

        strm.clear_msg();
        assert!(strm.msg.is_none());
    }

    // --- Allocator abstraction (no `unsafe` at the call site) ----------------

    #[test]
    fn custom_allocator_allocate_and_deallocate_are_invoked() {
        let strm = ZStream::with_allocator(CountingAlloc::new());
        let alloc = strm.allocator();

        // Zero-sized requests honour the contract (count, but yield `None`).
        assert!(alloc.allocate(0, 8).is_none());
        assert!(alloc.allocate(8, 0).is_none());
        assert_eq!(alloc.allocs.get(), 2);

        // A non-zero request is counted and yields a pointer.
        let p = alloc.allocate(4, 16);
        assert!(p.is_some());
        assert_eq!(alloc.allocs.get(), 3);

        // `deallocate` is callable with **no `unsafe` block** and is counted.
        alloc.deallocate(p.unwrap(), 4, 16);
        assert_eq!(alloc.deallocs.get(), 1);
    }

    #[test]
    fn global_alloc_rejects_zero_and_overflow() {
        let a = GlobalAlloc;
        assert!(a.allocate(0, 8).is_none());
        assert!(a.allocate(8, 0).is_none());
        // `count * size` overflows `usize` -> `None` (no panic).
        assert!(a.allocate(usize::MAX, 2).is_none());
    }

    #[test]
    fn global_alloc_vends_a_real_nonnull_block() {
        // A small allocation succeeds and is non-null. (The block is reclaimed
        // by `crate::ffi` in production; this unit test deliberately leaks the
        // few bytes, which is harmless for a short-lived test process.)
        let a = GlobalAlloc;
        let p = a.allocate(8, 1);
        assert!(p.is_some());
    }

    // --- Internal-state ownership and lifecycle ------------------------------

    #[test]
    fn set_state_take_state_and_kind() {
        let mut strm = ZStream::new();
        assert!(!strm.has_state());

        let (state, _dropped, _resets) = drop_flag_state(StreamKind::Deflate);
        strm.set_state(state);
        assert!(strm.has_state());
        assert_eq!(strm.kind(), Some(StreamKind::Deflate));
        assert!(strm.state().unwrap().is_deflate());
        assert!(!strm.state().unwrap().is_inflate());

        let taken = strm.take_state();
        assert!(taken.is_some());
        assert!(!strm.has_state());
        assert_eq!(strm.kind(), None);
    }

    #[test]
    fn end_sets_state_to_none_and_drops_it() {
        let (state, dropped, _resets) = drop_flag_state(StreamKind::Inflate);
        let mut strm = ZStream::new();
        strm.set_state(state);
        assert!(strm.has_state());
        assert!(!dropped.get());

        strm.end();
        assert!(!strm.has_state());
        // `end()` dropped the boxed state, running its `Drop`.
        assert!(dropped.get());

        // Ending an already-ended stream is a harmless no-op.
        strm.end();
        assert!(!strm.has_state());
    }

    #[test]
    fn dropping_stream_runs_state_drop() {
        let (state, dropped, _resets) = drop_flag_state(StreamKind::Deflate);
        {
            let mut strm = ZStream::new();
            strm.set_state(state);
            assert!(!dropped.get());
            // `strm` goes out of scope here.
        }
        // Dropping the `ZStream` dropped the `Box<dyn StreamState>` -> RAII.
        assert!(dropped.get());
    }

    #[test]
    fn taking_state_then_dropping_box_runs_drop() {
        let (state, dropped, _resets) = drop_flag_state(StreamKind::Inflate);
        let mut strm = ZStream::new();
        strm.set_state(state);

        let taken = strm.take_state().expect("state installed");
        assert!(!dropped.get(), "moving out must not drop");
        drop(taken);
        assert!(dropped.get());
    }

    // --- Reset ---------------------------------------------------------------

    #[test]
    fn reset_zeroes_accounting_and_resets_state() {
        let (state, _dropped, resets) = drop_flag_state(StreamKind::Deflate);
        let mut strm = ZStream::new();
        strm.set_state(state);

        strm.add_total_in(123);
        strm.add_total_out(45);
        strm.set_avail_in(9);
        strm.set_avail_out(99);
        strm.set_msg("data error");

        strm.reset();

        assert_eq!(strm.total_in, 0);
        assert_eq!(strm.total_out, 0);
        assert_eq!(strm.avail_in(), 0);
        assert_eq!(strm.avail_out(), 0);
        assert!(strm.msg.is_none());
        // The installed state's `reset` was invoked exactly once.
        assert_eq!(resets.get(), 1);
        // The state itself is retained (reset, not ended).
        assert!(strm.has_state());
    }

    #[test]
    fn reset_without_state_is_fine() {
        let mut strm = ZStream::new();
        strm.add_total_in(7);
        strm.reset();
        assert_eq!(strm.total_in, 0);
        assert!(!strm.has_state());
    }

    // --- Trait helpers and Debug --------------------------------------------

    #[test]
    fn stream_kind_is_copy_and_eq() {
        let k = StreamKind::Deflate;
        let k2 = k; // Copy
        assert_eq!(k, k2);
        assert_ne!(StreamKind::Deflate, StreamKind::Inflate);
    }

    #[test]
    fn global_alloc_is_zero_sized_and_default() {
        assert_eq!(core::mem::size_of::<GlobalAlloc>(), 0);
        // Exercise the derived `Default` impl through a generic boundary so the
        // call site is not a concrete unit-struct `::default()` (which clippy's
        // `default_constructed_unit_structs` lint flags); this still proves
        // `GlobalAlloc: Default`.
        fn defaulted<T: Default>() -> T {
            T::default()
        }
        assert_eq!(GlobalAlloc, defaulted::<GlobalAlloc>());
    }

    #[test]
    fn debug_impl_renders_fields_without_requiring_alloc_debug() {
        let mut strm = ZStream::with_allocator(CountingAlloc::new());
        strm.add_total_in(3);
        let (state, _d, _r) = drop_flag_state(StreamKind::Inflate);
        strm.set_state(state);

        let rendered = format!("{strm:?}");
        assert!(rendered.contains("ZStream"));
        assert!(rendered.contains("total_in"));
        assert!(rendered.contains("Inflate")); // state kind is shown
    }
}
