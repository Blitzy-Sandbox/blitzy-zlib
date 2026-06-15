//! The idiomatic, ownership-enforcing stream abstraction — `zlib_rs`'s safe
//! analogue of the C `z_stream` structure.
//!
//! [`ZStream`] is the central data carrier threaded through every public
//! compression and decompression entry point. It replaces the *public surface*
//! of the C `z_stream` (declared in `zlib.h`, lines 93-111) while expressing the
//! library's memory-ownership model (AAP §0.6.3) directly in the Rust type
//! system: the internal compression/decompression state is an
//! [`Option<Box<dyn StreamState>>`] that the borrow checker frees deterministically
//! on [`Drop`], so `deflateEnd`/`inflateEnd` become a no-op assignment
//! ([`ZStream::end`]) and double-free / use-after-free are *unrepresentable*.
//!
//! # Relationship to the C `z_stream`
//!
//! The C structure interleaves raw I/O pointers, cumulative counters, an opaque
//! state pointer, a trio of allocator hooks, and two status fields:
//!
//! ```c
//! typedef struct z_stream_s {
//!     z_const Bytef *next_in;   /* next input byte */
//!     uInt     avail_in;        /* number of bytes available at next_in */
//!     uLong    total_in;        /* total number of input bytes read so far */
//!     Bytef    *next_out;       /* next output byte will go here */
//!     uInt     avail_out;       /* remaining free space at next_out */
//!     uLong    total_out;       /* total number of bytes output so far */
//!     z_const char *msg;        /* last error message, NULL if no error */
//!     struct internal_state FAR *state; /* not visible by applications */
//!     alloc_func zalloc;        /* used to allocate the internal state */
//!     free_func  zfree;         /* used to free the internal state */
//!     voidpf     opaque;        /* private data object passed to zalloc/zfree */
//!     int     data_type;        /* best guess about the data type */
//!     uLong   adler;            /* Adler-32 or CRC-32 of the uncompressed data */
//!     uLong   reserved;         /* reserved for future use */
//! } z_stream;
//! ```
//!
//! The idiomatic [`ZStream`] keeps the *observable* accounting and status fields,
//! folds the three pointer-shaped concerns (`state`, the `zalloc`/`zfree`/`opaque`
//! allocator triple, and the per-call `next_in`/`next_out` buffers) into safe Rust
//! constructs, and drops `reserved` entirely:
//!
//! | C field(s)                          | Rust representation                    | Notes |
//! |-------------------------------------|----------------------------------------|-------|
//! | `next_in` + `avail_in`              | per-call `&[u8]` slice (+ [`avail_in`]) | **not stored** — see *lifetime-free design* below |
//! | `total_in`                          | [`total_in: u64`]                       | `uLong`; widened to [`u64`] for 64-bit-clean totals |
//! | `next_out` + `avail_out`            | per-call `&mut [u8]` slice (+ [`avail_out`]) | **not stored** |
//! | `total_out`                         | [`total_out: u64`]                      | `uLong`; widened to [`u64`] |
//! | `msg`                               | [`msg: Option<&'static str>`]           | `char*`; points to the static `z_errmsg` table, so a `'static` borrow is exact and allocation-free |
//! | `state`                             | [`state`] (private `Option<Box<dyn StreamState>>`) | owned, freed on `Drop` |
//! | `zalloc` / `zfree` / `opaque`       | [`allocator: A`] (the [`Allocator`] trait) | abstracted; default [`GlobalAlloc`] |
//! | `data_type`                         | [`data_type: i32`]                      | `int`; best-guess for deflate, decode bits for inflate |
//! | `adler`                             | [`adler: u32`]                          | `uLong` but the value is always 32-bit |
//! | `reserved`                          | *(omitted)*                             | exists only in the `#[repr(C)] z_stream` in `crate::ffi` |
//!
//! [`avail_in`]: ZStream::avail_in
//! [`avail_out`]: ZStream::avail_out
//! [`total_in: u64`]: ZStream::total_in
//! [`total_out: u64`]: ZStream::total_out
//! [`msg: Option<&'static str>`]: ZStream::msg
//! [`state`]: ZStream::state
//! [`allocator: A`]: ZStream::allocator
//! [`data_type: i32`]: ZStream::data_type
//! [`adler: u32`]: ZStream::adler
//!
//! # The FFI twin
//!
//! This module defines only the **safe** representation. The actual C-ABI
//! `#[repr(C)] z_stream` — with all fourteen raw fields (including `reserved`)
//! and the raw `*mut Bytef` / `alloc_func` / `free_func` pointers — lives in
//! `crate::ffi`, which owns *all* `unsafe` pointer marshalling between the two
//! representations. Per AAP §0.6.2, `unsafe` in this crate is confined to
//! `crate::ffi` and `crate::inflate::fast`; **there is no `unsafe` block in this
//! file**.
//!
//! # Lifetime-free design
//!
//! `ZStream` deliberately does **not** store the input/output buffers. In C,
//! `next_in`/`next_out` are raw pointers that advance as bytes are consumed and
//! produced. Storing the equivalent Rust slices (`&[u8]`/`&mut [u8]`) inside the
//! struct would infect `ZStream` with lifetime parameters and make it nearly
//! impossible to hold across calls. Instead, the engine entry points
//! (`deflate`, `inflate`) take the buffers **per call** as `&[u8]` / `&mut [u8]`
//! arguments, exactly mirroring how a C caller resupplies `next_in`/`next_out`
//! on each invocation. `ZStream` retains only the *bookkeeping*: the cumulative
//! [`total_in`](ZStream::total_in)/[`total_out`](ZStream::total_out) counters and
//! the last-known [`avail_in`](ZStream::avail_in)/[`avail_out`](ZStream::avail_out)
//! remainders, which the engine updates after each call and which the FFI layer
//! mirrors back onto the C `z_stream`.
//!
//! # Avoiding a dependency cycle
//!
//! The deflate and inflate engines depend on this module (they consume
//! `ZStream`), never the other way around. To let `ZStream` own the engine's
//! private state *without* importing the engines, the state is stored behind the
//! object-safe [`StreamState`] trait as `Box<dyn StreamState>`. The concrete
//! `DeflateState` / `InflateState` types (in `crate::deflate::state` /
//! `crate::inflate::state`) implement [`StreamState`]; an engine recovers its
//! concrete state with the [`Any`]-based downcast helpers
//! [`ZStream::state_as`] / [`ZStream::state_as_mut`].
//!
//! # `no_std`
//!
//! `ZStream` owns heap data (the boxed state, and the buffers the allocator
//! hands out), so it requires an allocator but not the standard library. Under
//! the default `std` build, [`Box`]/[`Vec`] come from the prelude; under a
//! `no-std` build they come from the `alloc` crate (which the crate root brings
//! into scope with `extern crate alloc;`). This module references only [`core`]
//! and [`alloc`], never `std`, so it compiles unchanged in both configurations.
//!
//! # Source mapping
//!
//! | Item                       | Upstream origin                |
//! |----------------------------|--------------------------------|
//! | [`ZStream`]                | `zlib.h` `z_stream_s` L93-111  |
//! | [`Allocator`] / [`GlobalAlloc`] | `zlib.h` `alloc_func`/`free_func` L85-86 |
//! | [`StreamState`]            | `zlib.h` `struct internal_state` (opaque) |
//!
//! # Examples
//!
//! ```
//! use zlib_rs::stream::ZStream;
//!
//! // Pure-Rust callers use the global allocator and write `ZStream::new()`.
//! let mut strm = ZStream::new();
//! assert!(!strm.is_initialized()); // no engine state attached yet
//! assert_eq!(strm.total_in, 0);
//! assert_eq!(strm.total_out, 0);
//!
//! // `end()` releases any owned state (the idiomatic `deflateEnd`/`inflateEnd`).
//! strm.end();
//! assert!(!strm.is_initialized());
//! ```

// ===========================================================================
// Imports
// ===========================================================================
//
// Heap container types backing the owned state and the allocator's buffers.
// Under the default `std` build `Box`/`Vec` are in the prelude; under a
// `no-std` build they come from the `alloc` crate, which `lib.rs` brings into
// scope with `extern crate alloc;`.
#[cfg(not(feature = "std"))]
use alloc::{boxed::Box, vec::Vec};

// `Any` powers the safe downcast from `dyn StreamState` back to the concrete
// engine state (`DeflateState`/`InflateState`). It lives in `core`, so it is
// available in `no_std` builds.
use core::any::Any;

// The file's dependency whitelist is exactly `src/error.rs` + `src/constants.rs`.
// `ZlibError`/`err_msg` back the `msg` helpers; `DataType`/`Z_UNKNOWN` back the
// `data_type` field and its classification accessor.
use crate::constants::{DataType, Z_UNKNOWN};
use crate::error::{ZlibError, err_msg};

// ===========================================================================
// StreamKind — which engine owns the boxed state
// ===========================================================================

/// Identifies which compression engine a [`ZStream`]'s internal state belongs
/// to.
///
/// A [`ZStream`] is format-agnostic at construction; once an engine attaches its
/// state (via [`ZStream::set_state`]) the kind is reported by
/// [`StreamState::kind`] and surfaced through [`ZStream::kind`]. This lets the
/// FFI layer and generic helpers branch on the active direction without
/// downcasting to a concrete state type.
///
/// # Examples
///
/// ```
/// use zlib_rs::stream::StreamKind;
///
/// // `StreamKind` is a plain, copyable tag.
/// let k = StreamKind::Deflate;
/// assert_eq!(k, StreamKind::Deflate);
/// assert_ne!(k, StreamKind::Inflate);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StreamKind {
    /// The stream holds compression (deflate) state.
    Deflate,
    /// The stream holds decompression (inflate) state.
    Inflate,
}

// ===========================================================================
// StreamState — the object-safe interface to the boxed engine state
// ===========================================================================

/// The object-safe interface implemented by the engines' private state structs.
///
/// `ZStream` owns its internal state as a `Box<dyn StreamState>` so it can carry
/// either compression or decompression state **without** this module depending
/// on the `crate::deflate` / `crate::inflate` engines (which would create a
/// dependency cycle — the engines depend on `stream`, not the reverse). The
/// concrete `DeflateState` and `InflateState` types implement this trait.
///
/// The trait is intentionally minimal: it exposes only what a *format-agnostic*
/// owner needs — the [`kind`](StreamState::kind), an idiomatic [`reset`] hook,
/// and the [`Any`] upcasts that let an engine recover its concrete state.
///
/// # Implementing `StreamState`
///
/// Concrete state types provide the trivial [`as_any`]/[`as_any_mut`] bodies so
/// callers can downcast through [`ZStream::state_as`]:
///
/// ```
/// use core::any::Any;
/// use zlib_rs::stream::{StreamKind, StreamState};
///
/// #[derive(Debug)]
/// struct MyDeflateState {
///     level: i32,
/// }
///
/// impl StreamState for MyDeflateState {
///     fn kind(&self) -> StreamKind {
///         StreamKind::Deflate
///     }
///     fn reset(&mut self) {
///         self.level = 6; // restore the default level
///     }
///     fn as_any(&self) -> &dyn Any {
///         self
///     }
///     fn as_any_mut(&mut self) -> &mut dyn Any {
///         self
///     }
/// }
///
/// let mut state = MyDeflateState { level: 0 };
/// state.reset();
/// assert_eq!(state.kind(), StreamKind::Deflate);
/// assert_eq!(state.level, 6);
/// ```
///
/// [`reset`]: StreamState::reset
/// [`as_any`]: StreamState::as_any
/// [`as_any_mut`]: StreamState::as_any_mut
///
/// # Object safety
///
/// Every method takes `&self`/`&mut self` and returns no `Self`-typed or generic
/// value, so `dyn StreamState` is a valid trait object. The `'static` supertrait
/// guarantees the state owns all its data (no borrowed references), which is
/// what makes the [`Any`]-based downcasts sound.
pub trait StreamState: 'static {
    /// Returns whether this state drives compression or decompression.
    fn kind(&self) -> StreamKind;

    /// Re-initializes the engine state for stream reuse, mirroring the internal
    /// reset performed by `deflateReset`/`inflateReset`.
    ///
    /// This resets only the *engine-private* state; the stream-level accounting
    /// (totals, `msg`, `adler`, `data_type`) is reset by [`ZStream::reset`],
    /// which calls this method after clearing its own fields.
    fn reset(&mut self);

    /// Upcasts to [`&dyn Any`](Any) so an engine can downcast back to its
    /// concrete state type. Implementors return `self`.
    fn as_any(&self) -> &dyn Any;

    /// Mutable counterpart of [`as_any`](StreamState::as_any). Implementors
    /// return `self`.
    fn as_any_mut(&mut self) -> &mut dyn Any;
}

// ===========================================================================
// Allocator — the safe abstraction over the C zalloc/zfree/opaque hooks
// ===========================================================================

/// Abstraction over the working-memory allocator, preserving the semantics of
/// the C `zalloc`/`zfree`/`opaque` hooks (AAP §0.6.3) while remaining **fully
/// safe**.
///
/// In C, a `z_stream` carries three allocator fields:
///
/// ```c
/// typedef voidpf (*alloc_func)(voidpf opaque, uInt items, uInt size);
/// typedef void   (*free_func)(voidpf opaque, voidpf address);
/// ```
///
/// This trait captures the same contract idiomatically:
///
/// * [`allocate`](Allocator::allocate) corresponds to
///   `zalloc(opaque, items, size)` — it produces a block of `items * size`
///   bytes;
/// * [`deallocate`](Allocator::deallocate) corresponds to
///   `zfree(opaque, address)` — it releases a block previously produced by
///   `allocate`;
/// * the `opaque` context pointer is replaced by `&self`, so a stateful
///   allocator simply stores its context in the implementing type.
///
/// # Why owned `Box<[u8]>` instead of raw pointers
///
/// The trait deals in **owned** [`Box<[u8]>`] buffers rather than raw
/// `NonNull<u8>` pointers. This is a deliberate design choice that keeps the
/// safe core free of `unsafe` (AAP §0.6.2): ownership of every allocation is
/// tracked by the type system and released deterministically on [`Drop`], so
/// there is no raw pointer to dereference and no manual free that could leak,
/// double-free, or use-after-free. The default [`GlobalAlloc`] therefore needs
/// no `unsafe` whatsoever.
///
/// The raw, ABI-level `zalloc`/`zfree`/`opaque` function pointers required for
/// C callers that supply a *custom* allocator are marshalled in `crate::ffi`
/// (the designated `unsafe` boundary), which adapts those C function pointers to
/// an `Allocator` implementor. This module — the safe idiomatic twin — never
/// touches raw allocator pointers.
///
/// # Allocation failure
///
/// [`allocate`](Allocator::allocate) returns [`Option`]: [`None`] signals an
/// allocation failure (or an `items * size` multiplication overflow), which the
/// engine maps to `Z_MEM_ERROR`. Implementations must therefore report failure
/// by returning [`None`] rather than aborting, mirroring a C `zalloc` that
/// returns `Z_NULL`.
///
/// # Examples
///
/// ```
/// use zlib_rs::stream::{Allocator, GlobalAlloc};
///
/// let alloc = GlobalAlloc;
/// let buf = alloc.allocate(4, 8).expect("32-byte allocation");
/// assert_eq!(buf.len(), 32);
/// assert!(buf.iter().all(|&b| b == 0)); // freshly zeroed
/// alloc.deallocate(buf); // optional: ownership alone would free it on drop
/// ```
pub trait Allocator {
    /// Allocates a zeroed block of `items * size` bytes, mirroring
    /// `zalloc(opaque, items, size)`.
    ///
    /// Returns [`Some`] with an owned buffer of exactly `items * size` bytes, or
    /// [`None`] if the multiplication overflows [`usize`] or the underlying
    /// allocation fails. The returned bytes are zero-initialized, which is at
    /// least as strong as the C contract (whose `zalloc` returns uninitialized
    /// memory that the engine then populates).
    fn allocate(&self, items: usize, size: usize) -> Option<Box<[u8]>>;

    /// Releases a block previously produced by [`allocate`](Allocator::allocate),
    /// mirroring `zfree(opaque, address)`.
    ///
    /// The default implementation simply drops `buffer`, which releases it
    /// through Rust's ownership system. Custom allocators (for example, an
    /// arena or a C-supplied `zfree`) override this to route the release through
    /// their own bookkeeping. Because the buffer is taken **by value**, calling
    /// this method consumes the allocation and statically prevents any further
    /// use of it.
    fn deallocate(&self, buffer: Box<[u8]>) {
        drop(buffer);
    }
}

/// The default [`Allocator`]: a zero-sized handle to Rust's global allocator.
///
/// `GlobalAlloc` is the `A` type parameter that pure-Rust callers get for free
/// via [`ZStream::new`]. It is a unit struct, so threading it through
/// `ZStream<GlobalAlloc>` adds no runtime cost. Allocation is performed with
/// safe [`Vec`]/[`Box`] operations, and deallocation happens automatically when
/// the returned [`Box<[u8]>`] is dropped — there is no `unsafe` code involved.
///
/// # Examples
///
/// ```
/// use zlib_rs::stream::{Allocator, GlobalAlloc};
///
/// // Zero-sized: it carries no state, only the choice of allocator.
/// assert_eq!(core::mem::size_of::<GlobalAlloc>(), 0);
///
/// let buf = GlobalAlloc.allocate(16, 1).unwrap();
/// assert_eq!(buf.len(), 16);
/// ```
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GlobalAlloc;

impl Allocator for GlobalAlloc {
    /// Allocates `items * size` zeroed bytes from the global allocator.
    ///
    /// Uses [`Vec::try_reserve_exact`] so an allocation failure surfaces as
    /// [`None`] (mapped to `Z_MEM_ERROR` by the engine) instead of aborting the
    /// process, and [`usize::checked_mul`] so an `items * size` overflow is
    /// likewise reported as [`None`].
    fn allocate(&self, items: usize, size: usize) -> Option<Box<[u8]>> {
        // Reject overflowing requests up front (a C `zalloc` would receive a
        // wrapped size and under-allocate; returning `None` is the safe analogue
        // of `zalloc` returning `Z_NULL`).
        let total = items.checked_mul(size)?;

        // `try_reserve_exact` lets us recover from OOM gracefully rather than
        // aborting (which `Vec::with_capacity` / `vec![0; n]` would do).
        let mut buffer: Vec<u8> = Vec::new();
        buffer.try_reserve_exact(total).ok()?;

        // The reservation guarantees capacity, so this `resize` never
        // reallocates; it just fills the requested length with zeros.
        buffer.resize(total, 0u8);

        Some(buffer.into_boxed_slice())
    }

    // `deallocate` uses the trait default (drop): the global allocator reclaims
    // the block when the `Box<[u8]>` is dropped.
}

// ===========================================================================
// ZStream — the idiomatic stream
// ===========================================================================

/// The idiomatic, ownership-enforcing stream — `zlib_rs`'s safe analogue of the
/// C `z_stream` (see the [module documentation](self) for the full field
/// correspondence and design rationale).
///
/// `ZStream` carries the observable accounting and status of a compression or
/// decompression operation and owns the engine's private state behind the
/// object-safe [`StreamState`] trait. It is generic over an [`Allocator`] `A`,
/// which defaults to [`GlobalAlloc`] so pure-Rust callers can simply write
/// [`ZStream::new`].
///
/// The input/output buffers are intentionally **not** stored (see *lifetime-free
/// design* in the [module docs](self)); the engine entry points take them per
/// call as `&[u8]` / `&mut [u8]`, and this struct tracks only the cumulative and
/// last-known counters.
///
/// # Examples
///
/// ```
/// use zlib_rs::stream::ZStream;
///
/// let mut strm = ZStream::new();
/// strm.set_data_type(0); // Z_BINARY
/// assert_eq!(strm.data_type, 0);
/// assert!(!strm.is_initialized());
/// ```
pub struct ZStream<A: Allocator = GlobalAlloc> {
    /// Total number of input bytes read so far (C `total_in`, `uLong`).
    ///
    /// Widened to [`u64`] so the counter is correct on 64-bit platforms even
    /// where C `uLong` is 32 bits. The engine increments this by the number of
    /// input bytes consumed on each `deflate`/`inflate` call.
    pub total_in: u64,

    /// Total number of output bytes produced so far (C `total_out`, `uLong`).
    ///
    /// Widened to [`u64`] for the same reason as [`total_in`](ZStream::total_in).
    pub total_out: u64,

    /// Bytes still available in the most recently supplied input buffer
    /// (C `avail_in`, `uInt`).
    ///
    /// This is *bookkeeping*, not storage: the buffer itself is passed per call.
    /// The engine sets this to the number of unconsumed input bytes after a
    /// call, and the FFI layer mirrors it back onto the C `z_stream.avail_in`.
    pub avail_in: u32,

    /// Free space still remaining in the most recently supplied output buffer
    /// (C `avail_out`, `uInt`).
    ///
    /// As with [`avail_in`](ZStream::avail_in), this is bookkeeping for the
    /// per-call output slice; the engine updates it after each call.
    pub avail_out: u32,

    /// The last error or status message, or [`None`] when there is no message
    /// (C `msg`, `char*` — `NULL` if no error).
    ///
    /// Modeled as `Option<&'static str>` because canonical zlib only ever points
    /// `msg` at entries of the static `z_errmsg` table (reproduced by
    /// [`crate::error::err_msg`]); a `'static` borrow is therefore exact and
    /// requires no allocation. Use [`set_msg_from_err`](ZStream::set_msg_from_err)
    /// or [`set_msg_from_code`](ZStream::set_msg_from_code) to populate it.
    pub msg: Option<&'static str>,

    /// The engine's private state, or [`None`] before initialization / after
    /// [`end`](ZStream::end) (C `state`, the opaque `struct internal_state *`).
    ///
    /// Owning this as a `Box<dyn StreamState>` is the heart of the memory model
    /// (AAP §0.6.3): the boxed state owns every working buffer (`window`,
    /// `pending_buf`, the Huffman tables, …), and dropping the `ZStream` — or
    /// calling [`end`](ZStream::end) — frees them all deterministically. This is
    /// the idiomatic replacement for `deflateEnd`/`inflateEnd`, with no manual
    /// free and no possibility of a double-free.
    state: Option<Box<dyn StreamState>>,

    /// Best guess about the data type (C `data_type`, `int`).
    ///
    /// For deflate this is one of `Z_BINARY`/`Z_TEXT`/`Z_UNKNOWN` (see
    /// [`data_type_classification`](ZStream::data_type_classification)); for
    /// inflate it encodes the decoder's bit position and block flags. It
    /// defaults to [`Z_UNKNOWN`] at construction, matching `deflate.c`'s
    /// `deflateResetKeep`.
    pub data_type: i32,

    /// The Adler-32 or CRC-32 checksum of the uncompressed data (C `adler`,
    /// `uLong`).
    ///
    /// Stored as [`u32`] because the value is always 32-bit. It is seeded by the
    /// engine on initialization (`1` for the zlib/Adler-32 framing, `0` for the
    /// gzip/CRC-32 and raw framings) and updated as data flows through.
    pub adler: u32,

    /// The working-memory allocator (the C `zalloc`/`zfree`/`opaque` triple,
    /// abstracted behind the [`Allocator`] trait).
    ///
    /// Private because it is an implementation detail of how buffers are
    /// obtained; read access is available through [`allocator`](ZStream::allocator).
    allocator: A,
}

// ---------------------------------------------------------------------------
// Construction with the default global allocator
// ---------------------------------------------------------------------------

impl ZStream<GlobalAlloc> {
    /// Creates a new, uninitialized stream backed by the global allocator
    /// ([`GlobalAlloc`]).
    ///
    /// The result has no engine state attached
    /// ([`is_initialized`](ZStream::is_initialized) is `false`), zeroed
    /// [`total_in`](ZStream::total_in)/[`total_out`](ZStream::total_out)/[`avail_in`](ZStream::avail_in)/[`avail_out`](ZStream::avail_out),
    /// no [`msg`](ZStream::msg), [`data_type`](ZStream::data_type) equal to
    /// [`Z_UNKNOWN`], and [`adler`](ZStream::adler) equal to `0` (the engine
    /// re-seeds the checksum on initialization). An engine attaches its state
    /// later with [`set_state`](ZStream::set_state).
    ///
    /// # Examples
    ///
    /// ```
    /// use zlib_rs::stream::ZStream;
    ///
    /// let strm = ZStream::new();
    /// assert!(!strm.is_initialized());
    /// assert_eq!(strm.total_in, 0);
    /// assert_eq!(strm.adler, 0);
    /// ```
    #[must_use]
    pub fn new() -> Self {
        Self::with_allocator(GlobalAlloc)
    }
}

impl Default for ZStream<GlobalAlloc> {
    /// Identical to [`ZStream::new`].
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Generic construction, accessors, state management, and lifecycle
// ---------------------------------------------------------------------------

impl<A: Allocator> ZStream<A> {
    /// Creates a new, uninitialized stream backed by a caller-supplied
    /// [`Allocator`].
    ///
    /// This is the generic constructor used when a custom allocator is required
    /// (for example, the FFI layer bridging a C-supplied `zalloc`/`zfree`).
    /// Pure-Rust callers should prefer [`ZStream::new`], which supplies
    /// [`GlobalAlloc`].
    ///
    /// # Examples
    ///
    /// ```
    /// use zlib_rs::stream::{GlobalAlloc, ZStream};
    ///
    /// // Explicitly choose the global allocator (equivalent to `ZStream::new`).
    /// let strm = ZStream::with_allocator(GlobalAlloc);
    /// assert!(!strm.is_initialized());
    /// ```
    #[must_use]
    pub fn with_allocator(allocator: A) -> Self {
        Self {
            total_in: 0,
            total_out: 0,
            avail_in: 0,
            avail_out: 0,
            msg: None,
            state: None,
            // Matches `deflate.c` `deflateResetKeep`, which sets
            // `strm->data_type = Z_UNKNOWN` before any data is processed.
            data_type: Z_UNKNOWN,
            // The engine seeds the real checksum on init (1 for zlib, 0 for
            // gzip/raw); `0` is the neutral pre-init default.
            adler: 0,
            allocator,
        }
    }

    // -----------------------------------------------------------------------
    // Allocator access
    // -----------------------------------------------------------------------

    /// Returns a shared reference to the stream's [`Allocator`].
    ///
    /// The engine uses this to obtain working buffers, and the FFI layer uses it
    /// to route allocations through a C-supplied allocator.
    #[must_use]
    pub fn allocator(&self) -> &A {
        &self.allocator
    }

    // -----------------------------------------------------------------------
    // Engine-state management
    // -----------------------------------------------------------------------

    /// Attaches engine state to the stream, replacing (and dropping) any state
    /// previously held.
    ///
    /// Called by `deflateInit2_` / `inflateInit2_` once they have constructed
    /// the concrete `DeflateState` / `InflateState`.
    pub fn set_state(&mut self, state: Box<dyn StreamState>) {
        self.state = Some(state);
    }

    /// Detaches and returns the engine state, leaving the stream uninitialized.
    ///
    /// Returns [`None`] if the stream had no state attached. Unlike
    /// [`end`](ZStream::end), this hands ownership of the state back to the
    /// caller instead of dropping it.
    #[must_use]
    pub fn take_state(&mut self) -> Option<Box<dyn StreamState>> {
        self.state.take()
    }

    /// Returns a shared reference to the engine state, if any is attached.
    #[must_use]
    pub fn state(&self) -> Option<&dyn StreamState> {
        self.state.as_deref()
    }

    /// Returns a mutable reference to the engine state, if any is attached.
    #[must_use]
    pub fn state_mut(&mut self) -> Option<&mut dyn StreamState> {
        self.state.as_deref_mut()
    }

    /// Returns `true` if engine state is currently attached (i.e. the stream has
    /// been initialized and not yet [`end`](ZStream::end)ed).
    #[must_use]
    pub fn is_initialized(&self) -> bool {
        self.state.is_some()
    }

    /// Returns the [`StreamKind`] of the attached state, or [`None`] if the
    /// stream is uninitialized.
    ///
    /// # Examples
    ///
    /// ```
    /// use zlib_rs::stream::ZStream;
    ///
    /// let strm = ZStream::new();
    /// assert_eq!(strm.kind(), None); // no state attached yet
    /// ```
    #[must_use]
    pub fn kind(&self) -> Option<StreamKind> {
        self.state.as_ref().map(|state| state.kind())
    }

    /// Downcasts the attached engine state to a concrete type `T`, returning a
    /// shared reference.
    ///
    /// Returns [`None`] if the stream is uninitialized or the attached state is
    /// not of type `T`. This is how an engine recovers its concrete
    /// `DeflateState` / `InflateState` from the type-erased
    /// `Box<dyn StreamState>`.
    #[must_use]
    pub fn state_as<T: StreamState>(&self) -> Option<&T> {
        self.state.as_ref()?.as_any().downcast_ref::<T>()
    }

    /// Mutable counterpart of [`state_as`](ZStream::state_as).
    #[must_use]
    pub fn state_as_mut<T: StreamState>(&mut self) -> Option<&mut T> {
        self.state.as_mut()?.as_any_mut().downcast_mut::<T>()
    }

    // -----------------------------------------------------------------------
    // Status / `data_type` / `msg` helpers
    // -----------------------------------------------------------------------

    /// Sets the [`data_type`](ZStream::data_type) field.
    ///
    /// `deflate` calls this to report its best guess (`Z_BINARY`/`Z_TEXT`) about
    /// the most recently processed input.
    pub fn set_data_type(&mut self, data_type: i32) {
        self.data_type = data_type;
    }

    /// Interprets [`data_type`](ZStream::data_type) as a deflate-side
    /// [`DataType`] classification.
    ///
    /// Returns [`Some`] for the canonical `Z_BINARY`/`Z_TEXT`/`Z_UNKNOWN` values
    /// and [`None`] otherwise (notably for the bit-field values inflate stores
    /// in `data_type`, which are not a [`DataType`]).
    ///
    /// # Examples
    ///
    /// ```
    /// use zlib_rs::constants::DataType;
    /// use zlib_rs::stream::ZStream;
    ///
    /// let mut strm = ZStream::new();
    /// strm.set_data_type(1); // Z_TEXT
    /// assert_eq!(strm.data_type_classification(), Some(DataType::Text));
    /// ```
    #[must_use]
    pub fn data_type_classification(&self) -> Option<DataType> {
        DataType::try_from(self.data_type).ok()
    }

    /// Sets [`msg`](ZStream::msg) to a static string.
    pub fn set_msg(&mut self, msg: &'static str) {
        self.msg = Some(msg);
    }

    /// Clears [`msg`](ZStream::msg) (the "no error" state, C `msg == NULL`).
    pub fn clear_msg(&mut self) {
        self.msg = None;
    }

    /// Sets [`msg`](ZStream::msg) to the canonical text of a [`ZlibError`].
    ///
    /// The message comes from [`ZlibError::message`], the same source the C
    /// library uses, so the text is identical to canonical zlib.
    ///
    /// # Examples
    ///
    /// ```
    /// use zlib_rs::error::ZlibError;
    /// use zlib_rs::stream::ZStream;
    ///
    /// let mut strm = ZStream::new();
    /// strm.set_msg_from_err(ZlibError::DataError);
    /// assert_eq!(strm.msg, Some("data error"));
    /// ```
    pub fn set_msg_from_err(&mut self, err: ZlibError) {
        self.msg = Some(err.message());
    }

    /// Sets [`msg`](ZStream::msg) from a raw C status code via
    /// [`crate::error::err_msg`].
    ///
    /// Codes whose canonical message is the empty string (notably `Z_OK`) clear
    /// the message to [`None`], matching zlib's convention of leaving `msg` null
    /// on success.
    ///
    /// # Examples
    ///
    /// ```
    /// use zlib_rs::stream::ZStream;
    ///
    /// let mut strm = ZStream::new();
    /// strm.set_msg_from_code(-3); // Z_DATA_ERROR
    /// assert_eq!(strm.msg, Some("data error"));
    ///
    /// strm.set_msg_from_code(0); // Z_OK -> empty message -> cleared
    /// assert_eq!(strm.msg, None);
    /// ```
    pub fn set_msg_from_code(&mut self, code: i32) {
        let message = err_msg(code);
        self.msg = if message.is_empty() {
            None
        } else {
            Some(message)
        };
    }

    // -----------------------------------------------------------------------
    // Lifecycle: reset and end
    // -----------------------------------------------------------------------

    /// Resets the stream-level accounting and delegates to the engine state's
    /// own reset, mirroring `deflateReset`/`inflateReset` at the idiomatic
    /// level.
    ///
    /// The cumulative counters, the per-call availability bookkeeping, the
    /// message, the checksum, and `data_type` are all cleared; if engine state
    /// is attached, its [`StreamState::reset`] is then invoked. The engine
    /// re-establishes the correct checksum seed (`1` for zlib, `0` for
    /// gzip/raw) when it next initializes or processes data.
    ///
    /// # Examples
    ///
    /// ```
    /// use zlib_rs::stream::ZStream;
    ///
    /// let mut strm = ZStream::new();
    /// strm.total_in = 123;
    /// strm.set_msg("stream error");
    /// strm.reset();
    /// assert_eq!(strm.total_in, 0);
    /// assert_eq!(strm.msg, None);
    /// ```
    pub fn reset(&mut self) {
        self.total_in = 0;
        self.total_out = 0;
        self.avail_in = 0;
        self.avail_out = 0;
        self.msg = None;
        self.data_type = Z_UNKNOWN;
        self.adler = 0;
        if let Some(state) = self.state.as_mut() {
            state.reset();
        }
    }

    /// Releases the engine state, returning the stream to the uninitialized
    /// condition — the idiomatic replacement for `deflateEnd`/`inflateEnd`.
    ///
    /// Setting [`state`](ZStream::state) to [`None`] drops the
    /// `Box<dyn StreamState>`, which frees every buffer the state owns. Because
    /// this is ordinary ownership, there is no manual free, no leak, and no way
    /// to double-free. Dropping the entire `ZStream` has the same effect, so an
    /// explicit `end()` is only needed when the stream value itself is reused.
    ///
    /// # Examples
    ///
    /// ```
    /// use zlib_rs::stream::ZStream;
    ///
    /// let mut strm = ZStream::new();
    /// // (an engine would attach state here)
    /// strm.end();
    /// assert!(!strm.is_initialized());
    /// ```
    pub fn end(&mut self) {
        self.state = None;
    }
}

// ---------------------------------------------------------------------------
// Debug
// ---------------------------------------------------------------------------

// Implemented by hand rather than derived so that:
//   * neither the allocator `A` nor the boxed `dyn StreamState` is required to
//     be `Debug` (a derived impl would add `A: Debug` and need
//     `dyn StreamState: Debug`); and
//   * the output is concise and useful — it summarizes the engine state as an
//     `initialized` flag plus its `kind`, instead of dumping the (large,
//     buffer-heavy) internal state.
impl<A: Allocator> core::fmt::Debug for ZStream<A> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ZStream")
            .field("total_in", &self.total_in)
            .field("total_out", &self.total_out)
            .field("avail_in", &self.avail_in)
            .field("avail_out", &self.avail_out)
            .field("msg", &self.msg)
            .field("data_type", &self.data_type)
            .field("adler", &self.adler)
            .field("initialized", &self.is_initialized())
            .field("kind", &self.kind())
            .finish()
    }
}

// ===========================================================================
// Tests
// ===========================================================================
//
// These cover the AAP validation matrix for `src/stream.rs`:
//   * `ZStream::new()` has no state, zeroed totals, `adler == 0`;
//   * `end()` releases the state;
//   * the default `A` is `GlobalAlloc`;
//   * a custom `Allocator` mock observes `allocate`/`deallocate` invocations;
//   * dropping a `ZStream` that holds state runs the state's `Drop`.
// plus exhaustive coverage of the allocator, the message helpers, `reset`, the
// downcast helpers, and the `Debug` output.

#[cfg(test)]
mod tests {
    use super::*;
    use core::cell::Cell;
    use core::sync::atomic::{AtomicUsize, Ordering};

    // -----------------------------------------------------------------------
    // Test doubles
    // -----------------------------------------------------------------------

    /// A mock engine state. Tracks `reset` invocations and, optionally, bumps a
    /// static counter when dropped so tests can prove deterministic teardown.
    #[derive(Debug)]
    struct MockState {
        kind: StreamKind,
        reset_calls: u32,
        drop_counter: Option<&'static AtomicUsize>,
    }

    impl MockState {
        fn new(kind: StreamKind) -> Self {
            Self {
                kind,
                reset_calls: 0,
                drop_counter: None,
            }
        }

        fn with_drop_counter(kind: StreamKind, counter: &'static AtomicUsize) -> Self {
            Self {
                kind,
                reset_calls: 0,
                drop_counter: Some(counter),
            }
        }
    }

    impl StreamState for MockState {
        fn kind(&self) -> StreamKind {
            self.kind
        }
        fn reset(&mut self) {
            self.reset_calls += 1;
        }
        fn as_any(&self) -> &dyn Any {
            self
        }
        fn as_any_mut(&mut self) -> &mut dyn Any {
            self
        }
    }

    impl Drop for MockState {
        fn drop(&mut self) {
            if let Some(counter) = self.drop_counter {
                counter.fetch_add(1, Ordering::SeqCst);
            }
        }
    }

    /// A second, distinct state type used to prove that `state_as` rejects the
    /// wrong concrete type.
    #[derive(Debug)]
    struct OtherState;

    impl StreamState for OtherState {
        fn kind(&self) -> StreamKind {
            StreamKind::Inflate
        }
        fn reset(&mut self) {}
        fn as_any(&self) -> &dyn Any {
            self
        }
        fn as_any_mut(&mut self) -> &mut dyn Any {
            self
        }
    }

    /// A custom allocator that counts `allocate`/`deallocate` calls (via
    /// interior mutability) while delegating the real work to [`GlobalAlloc`].
    #[derive(Debug, Default)]
    struct CountingAlloc {
        allocs: Cell<usize>,
        deallocs: Cell<usize>,
    }

    impl Allocator for CountingAlloc {
        fn allocate(&self, items: usize, size: usize) -> Option<Box<[u8]>> {
            self.allocs.set(self.allocs.get() + 1);
            GlobalAlloc.allocate(items, size)
        }
        fn deallocate(&self, buffer: Box<[u8]>) {
            self.deallocs.set(self.deallocs.get() + 1);
            drop(buffer);
        }
    }

    // -----------------------------------------------------------------------
    // Construction & defaults (explicit AAP validation cases)
    // -----------------------------------------------------------------------

    #[test]
    fn new_is_uninitialized_with_zeroed_accounting() {
        let strm = ZStream::new();
        assert!(strm.state.is_none());
        assert!(!strm.is_initialized());
        assert_eq!(strm.total_in, 0);
        assert_eq!(strm.total_out, 0);
        assert_eq!(strm.avail_in, 0);
        assert_eq!(strm.avail_out, 0);
        assert_eq!(strm.adler, 0);
        assert_eq!(strm.data_type, Z_UNKNOWN);
        assert_eq!(strm.msg, None);
        assert_eq!(strm.kind(), None);
    }

    #[test]
    fn default_matches_new() {
        let from_new = ZStream::new();
        let from_default = ZStream::default();
        assert_eq!(from_new.total_in, from_default.total_in);
        assert_eq!(from_new.data_type, from_default.data_type);
        assert_eq!(from_new.is_initialized(), from_default.is_initialized());
    }

    #[test]
    fn default_allocator_is_global_alloc() {
        let strm = ZStream::new();
        // Compiles only if the inferred `A` is `GlobalAlloc`.
        let allocator: &GlobalAlloc = strm.allocator();
        assert_eq!(*allocator, GlobalAlloc);
        // The default allocator is zero-sized, so it adds no per-stream cost.
        assert_eq!(core::mem::size_of::<GlobalAlloc>(), 0);
    }

    #[test]
    fn with_allocator_uses_supplied_allocator() {
        let strm = ZStream::with_allocator(CountingAlloc::default());
        assert_eq!(strm.allocator().allocs.get(), 0);
        assert!(!strm.is_initialized());
    }

    // -----------------------------------------------------------------------
    // Engine-state management
    // -----------------------------------------------------------------------

    #[test]
    fn set_and_take_state() {
        let mut strm = ZStream::new();
        assert!(strm.take_state().is_none());

        strm.set_state(Box::new(MockState::new(StreamKind::Deflate)));
        assert!(strm.is_initialized());
        assert_eq!(strm.kind(), Some(StreamKind::Deflate));

        let taken = strm.take_state();
        assert!(taken.is_some());
        assert!(!strm.is_initialized());
        assert_eq!(strm.kind(), None);
    }

    #[test]
    fn set_state_replaces_and_drops_previous_state() {
        static REPLACED_DROPS: AtomicUsize = AtomicUsize::new(0);
        let before = REPLACED_DROPS.load(Ordering::SeqCst);

        let mut strm = ZStream::new();
        strm.set_state(Box::new(MockState::with_drop_counter(
            StreamKind::Deflate,
            &REPLACED_DROPS,
        )));
        // Replacing the state must drop the previous box.
        strm.set_state(Box::new(OtherState));
        assert_eq!(REPLACED_DROPS.load(Ordering::SeqCst), before + 1);
        assert_eq!(strm.kind(), Some(StreamKind::Inflate));
    }

    #[test]
    fn state_and_state_mut_expose_attached_state() {
        let mut strm = ZStream::new();
        assert!(strm.state().is_none());
        assert!(strm.state_mut().is_none());

        strm.set_state(Box::new(MockState::new(StreamKind::Inflate)));
        assert_eq!(
            strm.state().map(StreamState::kind),
            Some(StreamKind::Inflate)
        );
        assert!(strm.state_mut().is_some());
    }

    #[test]
    fn state_as_downcasts_to_concrete_type() {
        let mut strm = ZStream::new();
        strm.set_state(Box::new(MockState::new(StreamKind::Deflate)));

        // Correct concrete type downcasts successfully.
        let concrete = strm.state_as::<MockState>().expect("downcast to MockState");
        assert_eq!(concrete.kind, StreamKind::Deflate);
        assert_eq!(concrete.reset_calls, 0);

        // The wrong concrete type yields `None`.
        assert!(strm.state_as::<OtherState>().is_none());
    }

    #[test]
    fn state_as_mut_allows_mutation() {
        let mut strm = ZStream::new();
        strm.set_state(Box::new(MockState::new(StreamKind::Deflate)));

        {
            let concrete = strm.state_as_mut::<MockState>().expect("mutable downcast");
            concrete.reset_calls = 7;
        }
        assert_eq!(strm.state_as::<MockState>().unwrap().reset_calls, 7);
    }

    #[test]
    fn state_as_on_uninitialized_is_none() {
        let strm = ZStream::new();
        assert!(strm.state_as::<MockState>().is_none());
    }

    // -----------------------------------------------------------------------
    // Lifecycle: end() and Drop free the state (explicit AAP validation cases)
    // -----------------------------------------------------------------------

    #[test]
    fn end_releases_and_drops_state() {
        static END_DROPS: AtomicUsize = AtomicUsize::new(0);
        let before = END_DROPS.load(Ordering::SeqCst);

        let mut strm = ZStream::new();
        strm.set_state(Box::new(MockState::with_drop_counter(
            StreamKind::Deflate,
            &END_DROPS,
        )));
        assert!(strm.is_initialized());

        strm.end();
        assert!(!strm.is_initialized());
        assert!(strm.state.is_none());
        // `end()` dropped the boxed state exactly once.
        assert_eq!(END_DROPS.load(Ordering::SeqCst), before + 1);
    }

    #[test]
    fn dropping_stream_runs_state_drop() {
        static SCOPE_DROPS: AtomicUsize = AtomicUsize::new(0);
        let before = SCOPE_DROPS.load(Ordering::SeqCst);

        {
            let mut strm = ZStream::new();
            strm.set_state(Box::new(MockState::with_drop_counter(
                StreamKind::Inflate,
                &SCOPE_DROPS,
            )));
            assert!(strm.is_initialized());
        } // `strm` dropped here → boxed state dropped.

        assert_eq!(SCOPE_DROPS.load(Ordering::SeqCst), before + 1);
    }

    #[test]
    fn reset_clears_accounting_and_delegates_to_state() {
        let mut strm = ZStream::new();
        strm.set_state(Box::new(MockState::new(StreamKind::Deflate)));
        strm.total_in = 99;
        strm.total_out = 88;
        strm.avail_in = 7;
        strm.avail_out = 6;
        strm.adler = 0xDEAD_BEEF;
        strm.data_type = 1;
        strm.set_msg("stream error");

        strm.reset();

        assert_eq!(strm.total_in, 0);
        assert_eq!(strm.total_out, 0);
        assert_eq!(strm.avail_in, 0);
        assert_eq!(strm.avail_out, 0);
        assert_eq!(strm.adler, 0);
        assert_eq!(strm.data_type, Z_UNKNOWN);
        assert_eq!(strm.msg, None);
        // The engine state's own reset was invoked exactly once.
        assert_eq!(strm.state_as::<MockState>().unwrap().reset_calls, 1);
    }

    #[test]
    fn reset_without_state_is_safe() {
        let mut strm = ZStream::new();
        strm.total_in = 42;
        strm.reset();
        assert_eq!(strm.total_in, 0);
        assert!(!strm.is_initialized());
    }

    // -----------------------------------------------------------------------
    // Allocator behavior (explicit AAP validation case for the mock)
    // -----------------------------------------------------------------------

    #[test]
    fn custom_allocator_allocate_deallocate_invoked() {
        let strm = ZStream::with_allocator(CountingAlloc::default());

        let buf = strm.allocator().allocate(4, 8).expect("32-byte allocation");
        assert_eq!(buf.len(), 32);
        assert_eq!(strm.allocator().allocs.get(), 1);
        assert_eq!(strm.allocator().deallocs.get(), 0);

        strm.allocator().deallocate(buf);
        assert_eq!(strm.allocator().deallocs.get(), 1);
    }

    #[test]
    fn global_alloc_returns_zeroed_exact_size() {
        let buf = GlobalAlloc.allocate(10, 4).expect("40-byte allocation");
        assert_eq!(buf.len(), 40);
        assert!(buf.iter().all(|&b| b == 0));
    }

    #[test]
    fn global_alloc_zero_size_is_empty() {
        let buf = GlobalAlloc.allocate(0, 16).expect("zero-length allocation");
        assert_eq!(buf.len(), 0);
        let buf2 = GlobalAlloc.allocate(16, 0).expect("zero-length allocation");
        assert_eq!(buf2.len(), 0);
    }

    #[test]
    fn global_alloc_overflow_returns_none() {
        // `usize::MAX * 2` overflows, so the request must fail gracefully.
        assert!(GlobalAlloc.allocate(usize::MAX, 2).is_none());
    }

    #[test]
    fn allocator_default_deallocate_drops_buffer() {
        // Exercise the trait's default `deallocate` (GlobalAlloc does not
        // override it): it must accept ownership and return without panicking.
        let buf = GlobalAlloc.allocate(8, 1).unwrap();
        GlobalAlloc.deallocate(buf);
    }

    // -----------------------------------------------------------------------
    // data_type & msg helpers
    // -----------------------------------------------------------------------

    #[test]
    fn set_data_type_and_classification() {
        let mut strm = ZStream::new();
        assert_eq!(strm.data_type_classification(), Some(DataType::Unknown));

        strm.set_data_type(0);
        assert_eq!(strm.data_type, 0);
        assert_eq!(strm.data_type_classification(), Some(DataType::Binary));

        strm.set_data_type(1);
        assert_eq!(strm.data_type_classification(), Some(DataType::Text));

        // An inflate-style bit-field value is not a canonical `DataType`.
        strm.set_data_type(0x40);
        assert_eq!(strm.data_type_classification(), None);
    }

    #[test]
    fn msg_helpers_round_trip() {
        let mut strm = ZStream::new();
        assert_eq!(strm.msg, None);

        strm.set_msg("custom message");
        assert_eq!(strm.msg, Some("custom message"));

        strm.clear_msg();
        assert_eq!(strm.msg, None);

        strm.set_msg_from_err(ZlibError::MemError);
        assert_eq!(strm.msg, Some("insufficient memory"));

        strm.set_msg_from_code(-3); // Z_DATA_ERROR
        assert_eq!(strm.msg, Some("data error"));

        // Z_OK maps to the empty message, which clears `msg`.
        strm.set_msg_from_code(0);
        assert_eq!(strm.msg, None);
    }

    // -----------------------------------------------------------------------
    // StreamKind, kind(), and Debug
    // -----------------------------------------------------------------------

    #[test]
    fn stream_kind_equality() {
        assert_eq!(StreamKind::Deflate, StreamKind::Deflate);
        assert_eq!(StreamKind::Inflate, StreamKind::Inflate);
        assert_ne!(StreamKind::Deflate, StreamKind::Inflate);
    }

    #[test]
    fn kind_reflects_attached_state() {
        let mut strm = ZStream::new();
        assert_eq!(strm.kind(), None);
        strm.set_state(Box::new(MockState::new(StreamKind::Inflate)));
        assert_eq!(strm.kind(), Some(StreamKind::Inflate));
    }

    #[test]
    fn debug_is_concise_and_reports_initialization() {
        let mut strm = ZStream::new();
        let uninit = format!("{strm:?}");
        assert!(uninit.contains("ZStream"));
        assert!(uninit.contains("initialized: false"));
        assert!(uninit.contains("kind: None"));

        strm.set_state(Box::new(MockState::new(StreamKind::Deflate)));
        let init = format!("{strm:?}");
        assert!(init.contains("initialized: true"));
        assert!(init.contains("Deflate"));
    }
}
