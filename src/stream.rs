//! The idiomatic streaming state object [`ZStream`] — the safe-Rust port of the
//! C `z_stream` (`zlib.h` L88-L110) — together with the crate-local
//! [`Allocator`] abstraction that replaces the C `zalloc`/`zfree` hooks.
//!
//! `ZStream` is the **central owning handle** of the whole library and is
//! re-exported from the crate root (`lib.rs`). Every compression or
//! decompression operation borrows a `&mut ZStream`; the concrete engine logic
//! lives in [`crate::deflate`] and [`crate::inflate`], which operate on the
//! [`DeflateState`] / [`InflateState`] this handle owns.
//!
//! # Relationship to the C `z_stream`
//!
//! The C library models a stream as a `z_stream` struct that the caller
//! allocates and whose opaque `internal_state *state` pointer is filled in by
//! `deflateInit`/`inflateInit` and torn down by `deflateEnd`/`inflateEnd`. That
//! design has two classic hazards this port makes *unrepresentable*:
//!
//! * A `state` pointer that is dangling, `NULL`, or points at the *wrong* kind
//!   of engine (an inflate routine handed a deflate state, or vice-versa).
//! * A manual `zcfree` that is forgotten (leak) or performed twice
//!   (use-after-free / double-free).
//!
//! Here the engine state is owned through a [`StreamState`] enum that holds
//! *at most one* boxed engine (`None`, `Deflate(Box<…>)`, or
//! `Inflate(Box<…>)`), so "no state", "deflate state", and "inflate state" are
//! distinct, checked cases (AAP §0.3.2 Type-State / Ownership). Because the box
//! and every owned buffer inside it are released by `Drop`, RAII fully subsumes
//! `deflateEnd`/`inflateEnd` (AAP §0.3.2 RAII / §0.6.3 Memory Ownership Model) —
//! there is nothing for the caller to remember to free.
//!
//! ## Field mapping (`z_stream` → [`ZStream`])
//!
//! | C field (`z_stream`)          | Rust                                   |
//! |-------------------------------|----------------------------------------|
//! | `next_in` / `next_out`        | *not stored* — passed as slices to the deflate/inflate methods |
//! | `avail_in` / `avail_out`      | *not stored* — the length of those slices; reconstructed at the FFI boundary |
//! | `total_in` / `total_out`      | [`total_in`](ZStream::total_in) / [`total_out`](ZStream::total_out) (`u64`) |
//! | `msg`                         | [`msg`](ZStream::msg) (`Option<&'static str>`) |
//! | `state`                       | [`state`](ZStream::state) ([`StreamState`], owned) |
//! | `zalloc` / `zfree` / `opaque` | [`alloc`](ZStream::alloc) (`A: Allocator`) |
//! | `data_type`                   | [`data_type`](ZStream::data_type) (`i32`) |
//! | `adler`                       | [`adler`](ZStream::adler) (`u32`) |
//! | `reserved`                    | [`reserved`](ZStream::reserved) (`u32`, parity only) |
//!
//! The raw `next_in`/`avail_in`/`next_out`/`avail_out` cursor quadruple is
//! **not** mirrored as fields: the idiomatic API accepts input and output as
//! Rust slices (for example `deflate(&mut self, input: &[u8], output: &mut [u8],
//! flush: FlushMode)`), and the running [`total_in`](ZStream::total_in) /
//! [`total_out`](ZStream::total_out) counters record cumulative progress. When
//! the crate is driven through its C ABI, the `ffi` layer rebuilds those slices
//! from the caller's raw `z_stream.next_in`/`avail_in`/… on each call and writes
//! the advanced cursors back afterwards.
//!
//! # Allocation
//!
//! The C `z_stream` carries `zalloc`/`zfree` function pointers so callers can
//! supply a custom allocator (for arenas, memory accounting, or embedded
//! heaps). That capability is modelled by the [`Allocator`] trait: the default
//! [`DefaultAllocator`] routes through the Rust global allocator, while the
//! `ffi` layer supplies a [`CAllocator`](crate::ffi::CAllocator) implementation
//! that forwards to the caller's `zalloc`/`zfree` (AAP §0.6.3, "has-hook
//! clause"). Every working buffer the engines request —
//! [`allocate_zeroed`](Allocator::allocate_zeroed) — is returned as an
//! [`AllocBuffer`], a smart owned region that is **either** a global-allocator
//! [`Vec`] (the null-hook default, byte-for-byte the historical behavior)
//! **or** a foreign region obtained from the caller's `zalloc` and released
//! through their `zfree` when the box drops. The engines only ever see a slice
//! ([`Deref`]/[`DerefMut`]), so which backing store is in play is invisible to
//! the compression/decompression logic and requires **no** `unsafe` on their
//! side.
//!
//! # Safety, `no_std`
//!
//! This module contains **zero `unsafe`** (enforced by `#![deny(unsafe_code)]`
//! below). The raw-pointer work for a caller-`zalloc`'d region — invoking the C
//! `zalloc`/`zfree` function pointers, materializing a slice over the returned
//! memory, and freeing it on drop — is defined entirely in the sanctioned
//! [`crate::ffi::alloc`] zone behind the safe [`ForeignBuffer`] trait (M6). The
//! [`Foreign`](AllocBuffer::Foreign) arm holds a `Box<dyn ForeignBuffer<T>>` and
//! delegates every access to that safe interface. Consequently the **core
//! compression engine** (`src/deflate/**`) also contains **zero `unsafe`** (user
//! rule R3) — its buffer fields are [`AllocBuffer`]s accessed purely through
//! safe slice operations, and the compression-logic files
//! (`slow.rs`/`stored.rs`/`trees.rs`) remain under `#![deny(unsafe_code)]`.
//!
//! Allocation is **fallible** at the boundary: when a caller installs a bounded
//! allocator whose `zalloc` reports out-of-memory, buffer construction returns
//! [`None`] and the init paths surface `Z_MEM_ERROR` (M7) rather than silently
//! using the global allocator. The module is `no_std` + `alloc` compatible: it
//! references only `core`, `alloc`, and the crate's own modules.
#![deny(unsafe_code)]

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::ffi::{c_uint, c_void};
use core::fmt;
use core::ops::{Deref, DerefMut};

use crate::constants::DataType;
use crate::deflate::state::DeflateState;
use crate::error::ReturnCode;
use crate::inflate::state::InflateState;

// ===========================================================================
// Allocator abstraction (replaces the C `zalloc`/`zfree`/`opaque` triple)
// ===========================================================================

// ---------------------------------------------------------------------------
// Caller-allocator hook (the C `zalloc`/`zfree`/`opaque` triple)
// ---------------------------------------------------------------------------

/// C `voidpf (*alloc_func)(voidpf opaque, uInt items, uInt size)` — a caller's
/// allocation hook (`zlib.h` L85). Structurally identical to
/// [`crate::ffi::alloc_func`]'s inner function-pointer type.
pub type ZallocFn = unsafe extern "C" fn(*mut c_void, c_uint, c_uint) -> *mut c_void;

/// C `void (*free_func)(voidpf opaque, voidpf address)` — a caller's
/// deallocation hook (`zlib.h` L86).
pub type ZfreeFn = unsafe extern "C" fn(*mut c_void, *mut c_void);

/// A caller-supplied `zalloc`/`zfree`/`opaque` triple, or "none".
///
/// This is the plumbing that lets [`AllocBuffer`] route a working buffer's
/// storage through the caller's C allocator (AAP §0.6.3). It is a plain [`Copy`]
/// value carried by every [`Allocator`] (see [`Allocator::hook`]); the default
/// allocator carries [`AllocHook::none`], so the historical global-allocator
/// path is entirely unchanged.
///
/// A hook is *active* only when **both** `zalloc` and `zfree` are present — a
/// caller that supplies one without the other does not get a usable custom
/// allocator (matching zlib, which uses the built-in allocator unless both are
/// set), and allocation falls back to the global allocator.
#[derive(Clone, Copy)]
pub struct AllocHook {
    /// The caller's allocation hook (`z_stream.zalloc`), or `None`.
    zalloc: Option<ZallocFn>,
    /// The caller's deallocation hook (`z_stream.zfree`), or `None`.
    zfree: Option<ZfreeFn>,
    /// The caller's private cookie (`z_stream.opaque`), passed to both hooks.
    opaque: *mut c_void,
}

impl AllocHook {
    /// The "no custom allocator" hook: allocation uses the Rust global
    /// allocator, exactly as a C caller leaving `zalloc`/`zfree` as `Z_NULL`.
    #[inline]
    #[must_use]
    pub const fn none() -> Self {
        Self {
            zalloc: None,
            zfree: None,
            opaque: core::ptr::null_mut(),
        }
    }

    /// Builds a hook from a caller's `zalloc`/`zfree`/`opaque` triple.
    #[inline]
    #[must_use]
    pub const fn new(
        zalloc: Option<ZallocFn>,
        zfree: Option<ZfreeFn>,
        opaque: *mut c_void,
    ) -> Self {
        Self {
            zalloc,
            zfree,
            opaque,
        }
    }

    /// Whether this hook can back a foreign allocation (both halves present).
    #[inline]
    #[must_use]
    pub const fn is_active(&self) -> bool {
        self.zalloc.is_some() && self.zfree.is_some()
    }

    /// The caller's allocation hook, if present. Read by the sanctioned
    /// [`crate::ffi::alloc`] bridge to invoke `zalloc`.
    #[inline]
    #[must_use]
    pub(crate) const fn zalloc(&self) -> Option<ZallocFn> {
        self.zalloc
    }

    /// The caller's deallocation hook, if present. Read by the sanctioned
    /// [`crate::ffi::alloc`] bridge to invoke `zfree` on drop.
    #[inline]
    #[must_use]
    pub(crate) const fn zfree(&self) -> Option<ZfreeFn> {
        self.zfree
    }

    /// The caller's private cookie, forwarded verbatim to both hooks.
    #[inline]
    #[must_use]
    pub(crate) const fn opaque(&self) -> *mut c_void {
        self.opaque
    }

    /// Allocates a zero-initialized foreign buffer of `count` elements through
    /// this (active) hook, delegating to the sanctioned [`crate::ffi::alloc`]
    /// zone where the raw-pointer `unsafe` is confined (M6).
    ///
    /// Returns [`None`] when the caller's `zalloc` reports out-of-memory (or the
    /// size is unrepresentable); there is **no** global-allocator fallback, so
    /// the init paths can surface `Z_MEM_ERROR` (M7).
    #[inline]
    pub(crate) fn try_alloc_zeroed<T: Copy + Default + 'static>(
        &self,
        count: usize,
    ) -> Option<Box<dyn ForeignBuffer<T>>> {
        crate::ffi::alloc::try_alloc_foreign::<T>(*self, count)
    }
}

// ---------------------------------------------------------------------------
// ForeignBuffer — the safe interface to a caller-`zalloc`'d region
// ---------------------------------------------------------------------------

/// The safe interface this module uses to access a caller-`zalloc`'d working
/// buffer **without any `unsafe`**.
///
/// The sole implementor is `CForeignBuffer` in the sanctioned
/// [`crate::ffi::alloc`] zone, which confines the raw-pointer hook invocation,
/// the slice materialization, and the `zfree`-on-drop (migration
/// unsafe-isolation rule / AAP §0.6.2). [`AllocBuffer::Foreign`] holds one as a
/// `Box<dyn ForeignBuffer<T>>` and delegates [`Deref`]/[`DerefMut`]/[`Clone`] to
/// these methods, so neither this module nor the compression engines contain any
/// `unsafe` (M6).
pub trait ForeignBuffer<T: Copy + Default> {
    /// The buffer contents as a shared slice of `T`.
    fn as_slice(&self) -> &[T];

    /// The buffer contents as a unique (mutable) slice of `T`.
    fn as_mut_slice(&mut self) -> &mut [T];

    /// Deep-clones into a fresh foreign region allocated through the same hook
    /// (matching C `deflateCopy`), or [`None`] if that allocation reports OOM —
    /// in which case [`AllocBuffer::clone`] performs a sound global-allocator
    /// copy so [`Clone`] remains infallible.
    fn clone_foreign(&self) -> Option<Box<dyn ForeignBuffer<T>>>;
}

// ---------------------------------------------------------------------------
// AllocBuffer — an owned buffer that is either global- or caller-allocated
// ---------------------------------------------------------------------------

/// An owned, zero-initialized buffer of `T` that transparently derefs to a
/// slice, backed **either** by the Rust global allocator (an owned [`Vec`])
/// **or** by a caller-supplied C `zalloc`/`zfree` pair (AAP §0.6.3).
///
/// This is the return type of [`Allocator::allocate_zeroed`]. The compression
/// and decompression engines store their working buffers (window, `pending_buf`,
/// hash tables) as `AllocBuffer`s and use them purely as slices via
/// [`Deref`]/[`DerefMut`]; they never observe which backing store is active and
/// contain no `unsafe`.
///
/// # Backing-store selection
///
/// [`AllocBuffer::try_zeroed`] returns a [`Foreign`](Self::Foreign) region only
/// when the [`AllocHook`] is [active](AllocHook::is_active) and the element count
/// is non-zero; that region is produced by the sanctioned [`crate::ffi::alloc`]
/// zone. With no hook (or an empty request) it is an [`Owned`](Self::Owned)
/// [`Vec`], byte-for-byte reproducing the historical global-allocator behavior.
///
/// When an active hook's `zalloc` reports out-of-memory (or the request is an
/// unrepresentable size), [`try_zeroed`](Self::try_zeroed) returns [`None`]
/// rather than silently falling back to the global allocator; callers propagate
/// that as `Z_MEM_ERROR` (M7). The only place a foreign buffer degrades to an
/// owned one is [`Clone`] under memory pressure (see its docs), which must stay
/// infallible.
///
/// # Zeroing and `unsafe` isolation
///
/// A foreign region is zero-filled inside [`crate::ffi::alloc`], reproducing C
/// `zcalloc`'s post-`zalloc` `zmemzero`. `T` is bounded `Copy + Default`, and the
/// element types the engines request (`u8`, `u16`, `u32`) all have an all-zero
/// bit pattern equal to their [`Default`] value, so the zero-fill is a correct
/// initialization. **This module contains no `unsafe`**: every raw-pointer
/// operation for the [`Foreign`](Self::Foreign) arm is behind the safe
/// [`ForeignBuffer`] interface (M6).
pub enum AllocBuffer<T: Copy + Default> {
    /// Global-allocator storage (the default / null-hook path).
    Owned(Vec<T>),
    /// Caller-`zalloc`'d storage, accessed through the safe [`ForeignBuffer`]
    /// interface and released through the caller's `zfree` when the box drops.
    /// The concrete implementor lives in the sanctioned [`crate::ffi::alloc`]
    /// zone, so this module never touches the raw pointer.
    Foreign(Box<dyn ForeignBuffer<T>>),
}

impl<T: Copy + Default> AllocBuffer<T> {
    /// Allocates a zero-initialized buffer of `count` elements, routing through
    /// the caller's `zalloc` when `hook` is active (see the type-level
    /// [backing-store selection](AllocBuffer#backing-store-selection)).
    ///
    /// Returns [`None`] when an active hook's `zalloc` reports out-of-memory (or
    /// the requested size is unrepresentable in the C `uInt` hook ABI); callers
    /// propagate that as `Z_MEM_ERROR` (M7). The null-hook and empty-count paths
    /// always succeed via the global allocator, byte-for-byte reproducing the
    /// historical behavior. All raw-pointer work for the active-hook path is
    /// confined to the sanctioned [`crate::ffi::alloc`] zone (M6), so this method
    /// contains no `unsafe`.
    #[must_use]
    pub fn try_zeroed(count: usize, hook: AllocHook) -> Option<Self>
    where
        T: 'static,
    {
        // Fast path / default: no custom allocator, or an empty request.
        // `vec![T::default(); count]` matches C `zcalloc`'s zero fill and is the
        // exact historical (global-allocator) behavior.
        if !hook.is_active() || count == 0 {
            return Some(AllocBuffer::Owned(alloc::vec![T::default(); count]));
        }
        // Active hook: delegate to the sanctioned `ffi` allocator zone. `None`
        // (OOM / unrepresentable size) propagates — no global fallback (M7).
        hook.try_alloc_zeroed::<T>(count).map(AllocBuffer::Foreign)
    }

    /// Wraps an existing [`Vec`] as an [`Owned`](Self::Owned) buffer (used by
    /// tests and by call sites that build a buffer eagerly).
    #[inline]
    #[must_use]
    pub fn from_vec(vec: Vec<T>) -> Self {
        AllocBuffer::Owned(vec)
    }

    /// Number of elements (also reachable as `self.len()` through [`Deref`];
    /// provided inherently so call sites holding the buffer by value are clear).
    #[inline]
    #[must_use]
    pub fn len(&self) -> usize {
        match self {
            AllocBuffer::Owned(v) => v.len(),
            AllocBuffer::Foreign(b) => b.as_slice().len(),
        }
    }

    /// Whether the buffer has no elements.
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl<T: Copy + Default> Deref for AllocBuffer<T> {
    type Target = [T];
    #[inline]
    fn deref(&self) -> &[T] {
        match self {
            AllocBuffer::Owned(v) => v.as_slice(),
            // Delegates to the safe `ForeignBuffer` interface; the raw-pointer
            // slice materialization is confined to `crate::ffi::alloc` (M6).
            AllocBuffer::Foreign(b) => b.as_slice(),
        }
    }
}

impl<T: Copy + Default> DerefMut for AllocBuffer<T> {
    #[inline]
    fn deref_mut(&mut self) -> &mut [T] {
        match self {
            AllocBuffer::Owned(v) => v.as_mut_slice(),
            // As in `deref`, delegating to the safe `ForeignBuffer` interface.
            AllocBuffer::Foreign(b) => b.as_mut_slice(),
        }
    }
}

impl<T: Copy + Default> Clone for AllocBuffer<T> {
    /// Deep-clones the buffer. An [`Owned`](Self::Owned) buffer clones its
    /// [`Vec`]; a [`Foreign`](Self::Foreign) buffer allocates a fresh region
    /// through the same hook (re-invoking the caller's `zalloc`, matching C
    /// `deflateCopy` which `ZALLOC`s new buffers) via
    /// [`ForeignBuffer::clone_foreign`] and copies the contents across.
    ///
    /// [`Clone`] cannot fail, so if the caller's `zalloc` reports OOM during the
    /// copy, this degrades to a **sound global-allocator copy** of the live
    /// contents (the fresh buffer becomes [`Owned`](Self::Owned)). The data is
    /// correct; only the backing allocator differs. This is the single
    /// documented boundary where a foreign buffer may become owned — the fallible
    /// INIT paths ([`try_zeroed`](Self::try_zeroed)) are the M7 target and still
    /// surface `Z_MEM_ERROR`; only this infallible `deflateCopy`-style clone
    /// tolerates the fallback.
    fn clone(&self) -> Self {
        match self {
            AllocBuffer::Owned(v) => AllocBuffer::Owned(v.clone()),
            AllocBuffer::Foreign(b) => match b.clone_foreign() {
                Some(fresh) => AllocBuffer::Foreign(fresh),
                None => AllocBuffer::Owned(b.as_slice().to_vec()),
            },
        }
    }
}

impl<T: Copy + Default> Default for AllocBuffer<T> {
    /// An empty [`Owned`](Self::Owned) buffer (no allocation) — the analogue of
    /// C's `state->window = Z_NULL` "not yet allocated" sentinel.
    #[inline]
    fn default() -> Self {
        AllocBuffer::Owned(Vec::new())
    }
}

// NOTE: `AllocBuffer` needs no manual `Drop`. The `Owned` arm's `Vec` frees
// itself through the global allocator, and the `Foreign` arm's
// `Box<dyn ForeignBuffer<T>>` runs its implementor's `Drop` (in the sanctioned
// `crate::ffi::alloc` zone), which calls the caller's `zfree`. Both release paths
// are automatic, so no `unsafe` deallocation lives in this module (M6).

/// Abstraction over the source of the library's heap buffers, replacing the C
/// `alloc_func`/`free_func` pointers (`zlib.h` L85-L86).
///
/// zlib lets callers override allocation via
/// `voidpf (*zalloc)(voidpf opaque, uInt items, uInt size)` and
/// `void (*zfree)(voidpf opaque, voidpf address)`. This trait is the safe Rust
/// equivalent of that customization point: the compression and decompression
/// engines allocate their working buffers (the sliding window, `pending_buf`,
/// and the `head`/`prev` hash tables) through an `Allocator`, and free them
/// through the same value (or, for the default, simply by dropping the owned
/// [`Vec`]).
///
/// The default implementation, [`DefaultAllocator`], uses the Rust global
/// allocator and is what Rust-native callers get. The C drop-in path installs a
/// different implementation in [`crate::ffi`] that forwards to the caller's
/// `zalloc`/`zfree`; that implementation is where the `unsafe` raw-pointer hook
/// invocation is confined. **This module intentionally defines only the trait
/// and the safe global default** — no `unsafe` appears here.
///
/// # Contract
///
/// Implementations must return a buffer of exactly `count` elements from
/// [`allocate_zeroed`](Allocator::allocate_zeroed), every element initialised to
/// its [`Default`] (numerically zero) value — mirroring the zero-fill that C
/// `zcalloc` performs. [`deallocate`](Allocator::deallocate) consumes a buffer
/// previously produced by the same allocator.
pub trait Allocator {
    /// Allocates a contiguous, zero-initialised buffer of `count` elements of
    /// type `T`, returned as an [`AllocBuffer<T>`].
    ///
    /// This is the safe analogue of the C `ZALLOC` macro (which calls the
    /// caller's `zalloc`, then zeroes the region). The returned [`AllocBuffer`]
    /// derefs to a `[T]` slice and, when this allocator carries an active
    /// [`hook`](Allocator::hook), is backed by the caller's `zalloc`/`zfree`;
    /// otherwise it is a global-allocator [`Vec`]. `T` is constrained to
    /// [`Copy`] + [`Default`] so the buffer can be filled with the type's zero
    /// value without running arbitrary drop or clone logic; the buffer element
    /// types the engines request are the plain integer types `u8`, `u16`, and
    /// `u32`, all of whose `Default` is `0`.
    ///
    /// Returns [`None`] when an active hook's `zalloc` reports out-of-memory;
    /// callers translate that into `Z_MEM_ERROR` (M7). The global-allocator
    /// default is infallible (it aborts on OOM, per Rust convention) and so
    /// always returns [`Some`].
    fn allocate_zeroed<T>(&self, count: usize) -> Option<AllocBuffer<T>>
    where
        T: Copy + Default + 'static;

    /// Returns the caller-allocator [`AllocHook`] this allocator forwards to,
    /// or [`AllocHook::none`] (the default) when it uses the global allocator.
    ///
    /// The deflate/inflate init paths read this so a state's lazily- or
    /// eagerly-allocated working buffers can be routed through the caller's
    /// `zalloc`/`zfree` (AAP §0.6.3).
    #[inline]
    fn hook(&self) -> AllocHook {
        AllocHook::none()
    }

    /// Releases a buffer previously produced by
    /// [`allocate_zeroed`](Allocator::allocate_zeroed).
    ///
    /// This is the safe analogue of the C `ZFREE` macro. Because an
    /// [`AllocBuffer`] releases its own storage on [`Drop`] — a global [`Vec`]
    /// through the global allocator, or a foreign region through the caller's
    /// `zfree` — the default implementation simply drops it. The method is
    /// retained for call-site clarity and API symmetry with the C `ZFREE`.
    #[inline]
    fn deallocate<T>(&self, buffer: AllocBuffer<T>)
    where
        T: Copy + Default,
    {
        // Dropping the `AllocBuffer` runs its `Drop`, which routes to the
        // correct deallocator (global for `Owned`, the caller's `zfree` for
        // `Foreign`). Made explicit for clarity.
        drop(buffer);
    }
}

/// The default [`Allocator`], backed by the Rust global allocator.
///
/// This zero-sized type is installed on every [`ZStream`] created without an
/// explicit allocator (see [`ZStream::new`]). It corresponds to a C caller that
/// left `z_stream.zalloc`/`zfree` as `Z_NULL`, in which case zlib falls back to
/// its built-in `zcalloc`/`zcfree` (ultimately `malloc`/`free`). Here the
/// "built-in" allocator is Rust's global allocator, reached through the standard
/// [`Vec`] APIs.
#[derive(Copy, Clone, Debug, Default)]
pub struct DefaultAllocator;

impl Allocator for DefaultAllocator {
    #[inline]
    fn allocate_zeroed<T>(&self, count: usize) -> Option<AllocBuffer<T>>
    where
        T: Copy + Default + 'static,
    {
        // The default allocator has no hook, so this is always an owned,
        // global-allocator `Vec` (byte-for-byte the historical behavior) and
        // therefore always `Some`. Routing through `try_zeroed` with the "none"
        // hook keeps the single source of truth for buffer construction.
        AllocBuffer::try_zeroed(count, AllocHook::none())
    }

    // `hook` uses the trait default (`AllocHook::none`): the global allocator.
}

// ===========================================================================
// StreamState — the owned engine state (replaces the C `internal_state *`)
// ===========================================================================

/// The engine state owned by a [`ZStream`], replacing the C
/// `internal_state *state` pointer (`zlib.h` L99).
///
/// A stream is in exactly one of three states, and the enum makes that a
/// type-level invariant:
///
/// * [`StreamState::None`] — not yet initialised for either direction
///   (equivalent to a C `state == Z_NULL`).
/// * [`StreamState::Deflate`] — owns a [`DeflateState`] for compression.
/// * [`StreamState::Inflate`] — owns an [`InflateState`] for decompression.
///
/// Modelling the two engines as one enum (rather than a pair of
/// `Option<Box<…>>` fields, the shape sketched in AAP §0.6.3) is a deliberate
/// tightening: it is *impossible* to simultaneously hold a deflate and an
/// inflate state, so the C hazard of dispatching an inflate routine on a deflate
/// state — or vice-versa — cannot be expressed. The [`Option`]-returning
/// accessors on [`ZStream`] ([`deflate_state`](ZStream::deflate_state),
/// [`inflate_state`](ZStream::inflate_state), …) recover the ergonomic
/// `Option<&…>` view the AAP describes.
///
/// The variant payloads are [`Box`]ed because the engine states are large
/// (deflate ~256 KB, inflate ~7 KB plus its window); boxing keeps `ZStream`
/// itself small and matches the `Option<Box<…>>` ownership the sibling engine
/// modules document.
#[derive(Default)]
pub(crate) enum StreamState {
    /// No engine has been initialised (C `state == Z_NULL`).
    #[default]
    None,
    /// A compression engine is installed.
    Deflate(Box<DeflateState>),
    /// A decompression engine is installed.
    Inflate(Box<InflateState>),
}

impl StreamState {
    /// Returns `true` if no engine is installed ([`StreamState::None`]).
    #[inline]
    #[must_use]
    pub(crate) fn is_none(&self) -> bool {
        matches!(self, StreamState::None)
    }

    /// Returns `true` if a compression engine is installed.
    #[inline]
    #[must_use]
    pub(crate) fn is_deflate(&self) -> bool {
        matches!(self, StreamState::Deflate(_))
    }

    /// Returns `true` if a decompression engine is installed.
    #[inline]
    #[must_use]
    pub(crate) fn is_inflate(&self) -> bool {
        matches!(self, StreamState::Inflate(_))
    }

    /// Returns a short, allocation-free label for the active variant, used by
    /// the [`fmt::Debug`] implementation (which must not print the — potentially
    /// non-`Debug` — engine state itself).
    #[inline]
    #[must_use]
    fn kind_str(&self) -> &'static str {
        match self {
            StreamState::None => "None",
            StreamState::Deflate(_) => "Deflate",
            StreamState::Inflate(_) => "Inflate",
        }
    }
}

impl fmt::Debug for StreamState {
    /// Prints only the variant name. Neither [`DeflateState`] nor
    /// [`InflateState`] implements [`Debug`] (they hold large working buffers
    /// whose contents are not useful to dump), so the payload is deliberately
    /// elided.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.kind_str())
    }
}

// ===========================================================================
// ZStream — the idiomatic port of the C `z_stream`
// ===========================================================================

/// The idiomatic streaming state, mirroring the C `z_stream`
/// (`zlib.h` L88-L110).
///
/// `ZStream` owns its engine state ([`StreamState`]) and its allocator (`A`),
/// so dropping a `ZStream` releases everything: the boxed engine and all of the
/// working buffers it owns. This is the direct replacement for C's
/// `internal_state *` plus the `zcalloc`/`zcfree`/`deflateEnd`/`inflateEnd`
/// bookkeeping (AAP §0.3.2 / §0.6.3).
///
/// The type parameter `A` selects the allocator; it defaults to
/// [`DefaultAllocator`] (the Rust global allocator) so Rust callers can simply
/// write `ZStream`. The C drop-in path in [`crate::ffi`] instantiates
/// `ZStream<CAllocator>` with an allocator that forwards to the caller's
/// `zalloc`/`zfree`.
///
/// # Cursor fields are intentionally absent
///
/// The C `next_in`/`avail_in`/`next_out`/`avail_out` cursor quadruple is *not*
/// stored. Input and output are passed as slices to the deflate/inflate methods,
/// and only the cumulative [`total_in`](Self::total_in) /
/// [`total_out`](Self::total_out) counters persist between calls. See the
/// [module documentation](crate::stream) for how the FFI layer bridges those
/// cursors.
///
/// # Examples
///
/// ```ignore
/// use zlib_rs::stream::ZStream;
///
/// // A freshly created stream owns no engine yet.
/// let strm = ZStream::new();
/// assert_eq!(strm.total_in, 0);
/// assert!(!strm.has_state());
/// // Dropping `strm` here needs no explicit `deflateEnd`/`inflateEnd`.
/// ```
pub struct ZStream<A: Allocator = DefaultAllocator> {
    /// Total number of input bytes consumed so far (C `uLong total_in`).
    ///
    /// Widened to [`u64`] so the counter is lossless on both 32- and 64-bit
    /// platforms, where C `uLong` is 32 or 64 bits respectively.
    pub total_in: u64,

    /// Total number of output bytes produced so far (C `uLong total_out`).
    ///
    /// Widened to [`u64`] for the same reason as [`total_in`](Self::total_in).
    pub total_out: u64,

    /// Best guess about the data type — binary or text for deflate, or the
    /// decoding state for inflate (C `int data_type`).
    ///
    /// The raw [`i32`] preserves the exact `Z_BINARY` / `Z_TEXT` / `Z_UNKNOWN`
    /// contract at the boundary; [`data_type_enum`](Self::data_type_enum)
    /// offers a typed [`DataType`] view when the value is one of those three.
    pub data_type: i32,

    /// Adler-32 or CRC-32 checksum of the uncompressed data (C `uLong adler`).
    ///
    /// Kept as [`u32`] because both checksums are 32-bit; the C field is
    /// `uLong` only because zlib predates fixed-width integer types.
    pub adler: u32,

    /// Last error message, or [`None`] when there is no error (C
    /// `z_const char *msg`, which is `NULL` when clear).
    ///
    /// Every message zlib can report is a compile-time-constant string (the C
    /// `z_errmsg` table reached through `ERR_MSG`), so a `&'static str` captures
    /// them without allocation and [`None`] models the C `NULL`. Populated via
    /// [`set_msg`](Self::set_msg); see [`ReturnCode::message`].
    pub msg: Option<&'static str>,

    /// Reserved for future use (C `uLong reserved`).
    ///
    /// Retained purely for conceptual parity with `z_stream`; the exact ABI
    /// width is reproduced only by the `#[repr(C)]` mirror in
    /// [`crate::ffi`]. Always `0` here.
    pub(crate) reserved: u32,

    /// The owned engine state (C `internal_state *state`).
    ///
    /// See [`StreamState`] for why this is an enum rather than a raw pointer.
    pub(crate) state: StreamState,

    /// The allocator backing this stream's buffers (C `zalloc`/`zfree`/`opaque`).
    ///
    /// See [`Allocator`] and [`DefaultAllocator`].
    pub(crate) alloc: A,
}

/// The Adler-32 checksum of the empty input (`adler32(0, Z_NULL, 0) == 1`).
///
/// This is the value reference zlib seeds `z_stream.adler` with for the default
/// (zlib-wrapper) framing. A freshly constructed [`ZStream`] adopts this seed;
/// installing or resetting an engine then overwrites [`adler`](ZStream::adler)
/// with the wrapper-appropriate value — this same `1` for Adler-32 / zlib, or
/// `0` for CRC-32 / gzip.
const ADLER32_INIT: u32 = 1;

impl ZStream<DefaultAllocator> {
    /// Creates a new stream backed by the [`DefaultAllocator`] (the Rust global
    /// allocator), with no engine installed.
    ///
    /// All counters start at `0`, [`data_type`](ZStream::data_type) at
    /// `Z_BINARY`, [`adler`](ZStream::adler) at the Adler-32 seed
    /// ([`ADLER32_INIT`]), and [`msg`](ZStream::msg) at [`None`]. This is the
    /// idiomatic counterpart to a zeroed C `z_stream` prior to
    /// `deflateInit`/`inflateInit`.
    #[must_use]
    pub fn new() -> Self {
        Self::with_allocator(DefaultAllocator)
    }
}

impl Default for ZStream<DefaultAllocator> {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

impl<A: Allocator> ZStream<A> {
    /// Creates a new stream backed by the supplied allocator, with no engine
    /// installed.
    ///
    /// This is the general constructor used by [`crate::ffi`] to build a
    /// `ZStream<CAllocator>` whose allocator forwards to the caller's
    /// `zalloc`/`zfree`. Field initialisation matches [`ZStream::new`].
    #[must_use]
    pub fn with_allocator(alloc: A) -> Self {
        ZStream {
            total_in: 0,
            total_out: 0,
            // `Z_BINARY` (0): the zeroed default a C caller would `memset` into
            // `z_stream`; the engine refines it to `Z_BINARY`/`Z_TEXT`
            // (deflate) or the decode state (inflate) as it runs.
            data_type: DataType::Binary.as_c_int(),
            adler: ADLER32_INIT,
            msg: None,
            reserved: 0,
            state: StreamState::None,
            alloc,
        }
    }

    /// Returns a shared reference to this stream's [`Allocator`].
    #[inline]
    #[must_use]
    pub fn allocator(&self) -> &A {
        &self.alloc
    }

    // -- engine-state management (used by the deflate/inflate init routines) --

    /// Installs a compression engine, replacing any previously installed state.
    ///
    /// Called by the deflate initialisation path once it has built the
    /// [`DeflateState`]. Any engine previously held is dropped (its buffers
    /// freed) as part of the assignment — the RAII replacement for the C
    /// `deflateEnd`/`inflateEnd` that would otherwise be required first.
    #[inline]
    pub(crate) fn set_deflate_state(&mut self, state: Box<DeflateState>) {
        self.state = StreamState::Deflate(state);
    }

    /// Installs a decompression engine, replacing any previously installed
    /// state. The dual of [`set_deflate_state`](Self::set_deflate_state).
    #[inline]
    pub(crate) fn set_inflate_state(&mut self, state: Box<InflateState>) {
        self.state = StreamState::Inflate(state);
    }

    /// Removes and returns the current engine state, leaving the stream in the
    /// [`StreamState::None`] state.
    ///
    /// Used by the `deflateEnd`/`inflateEnd` shims: taking the state and letting
    /// the returned value drop performs the teardown. (Simply calling
    /// [`clear_state`](Self::clear_state), or dropping the whole `ZStream`,
    /// achieves the same release.)
    #[inline]
    pub(crate) fn take_state(&mut self) -> StreamState {
        core::mem::take(&mut self.state)
    }

    /// Drops the current engine state, returning the stream to
    /// [`StreamState::None`].
    #[inline]
    pub(crate) fn clear_state(&mut self) {
        self.state = StreamState::None;
    }

    /// Borrows the installed compression engine, or [`None`] if the stream holds
    /// no state or an inflate state.
    #[inline]
    #[must_use]
    pub(crate) fn deflate_state(&self) -> Option<&DeflateState> {
        match &self.state {
            StreamState::Deflate(state) => Some(state),
            _ => None,
        }
    }

    /// Mutably borrows the installed compression engine, or [`None`]. The hot
    /// path for `deflate()`, which repeatedly advances the engine.
    #[inline]
    #[must_use]
    pub(crate) fn deflate_state_mut(&mut self) -> Option<&mut DeflateState> {
        match &mut self.state {
            StreamState::Deflate(state) => Some(state),
            _ => None,
        }
    }

    /// Borrows the installed decompression engine, or [`None`] if the stream
    /// holds no state or a deflate state.
    #[inline]
    #[must_use]
    pub(crate) fn inflate_state(&self) -> Option<&InflateState> {
        match &self.state {
            StreamState::Inflate(state) => Some(state),
            _ => None,
        }
    }

    /// Mutably borrows the installed decompression engine, or [`None`]. The hot
    /// path for `inflate()`.
    #[inline]
    #[must_use]
    pub(crate) fn inflate_state_mut(&mut self) -> Option<&mut InflateState> {
        match &mut self.state {
            StreamState::Inflate(state) => Some(state),
            _ => None,
        }
    }

    // -- public introspection -------------------------------------------------

    /// Returns `true` if an engine (deflate or inflate) is installed.
    #[inline]
    #[must_use]
    pub fn has_state(&self) -> bool {
        !self.state.is_none()
    }

    /// Returns `true` if this stream is initialised for compression.
    #[inline]
    #[must_use]
    pub fn is_deflate(&self) -> bool {
        self.state.is_deflate()
    }

    /// Returns `true` if this stream is initialised for decompression.
    #[inline]
    #[must_use]
    pub fn is_inflate(&self) -> bool {
        self.state.is_inflate()
    }

    /// Returns the typed [`DataType`] view of [`data_type`](Self::data_type),
    /// or [`None`] if the raw value is not one of `Z_BINARY` / `Z_TEXT` /
    /// `Z_UNKNOWN` (for example an inflate decode-state code).
    #[inline]
    #[must_use]
    pub fn data_type_enum(&self) -> Option<DataType> {
        DataType::from_c_int(self.data_type)
    }

    // -- error message bridging (mirrors the C `ERR_MSG` / `strm->msg`) --------

    /// Sets [`msg`](Self::msg) from a [`ReturnCode`], mirroring the C `ERR_MSG`
    /// macro that stores the `z_errmsg` string into `strm->msg`.
    ///
    /// A code whose message is empty (only [`ReturnCode::Ok`]) clears the field
    /// to [`None`], matching the C convention that `msg` is `NULL` when there is
    /// no error.
    pub fn set_msg(&mut self, code: ReturnCode) {
        let message = code.message();
        self.msg = if message.is_empty() {
            None
        } else {
            Some(message)
        };
    }

    /// Sets [`msg`](Self::msg) to an explicit static string.
    ///
    /// Used internally where a specific message is required that is not the
    /// canonical [`ReturnCode`] text (for example the deflate/inflate
    /// parameter-validation diagnostics).
    // Reserved internal API: consumed by this module's unit tests and intended
    // for the deflate/inflate parameter-validation diagnostics. Retained even in
    // build configurations that wire up no production caller, so the dead-code
    // lint is allowed narrowly here rather than dropping tested functionality.
    #[allow(dead_code)]
    #[inline]
    pub(crate) fn set_msg_str(&mut self, message: &'static str) {
        self.msg = Some(message);
    }

    /// Clears any error message, restoring [`msg`](Self::msg) to [`None`].
    #[inline]
    pub fn clear_msg(&mut self) {
        self.msg = None;
    }

    /// Resets the stream-level bookkeeping — the progress counters and the error
    /// message — without touching the engine state, allocator,
    /// [`data_type`](Self::data_type), or [`adler`](Self::adler).
    ///
    /// This is the shared portion of `deflateReset`/`inflateReset`: the engine
    /// resets its own internal fields (and sets `adler`/`data_type` to their
    /// wrapper-appropriate values), while these stream-level fields are cleared
    /// here.
    // Reserved internal API: consumed by this module's unit tests and intended
    // as the shared stream-level portion of deflateReset/inflateReset. Retained
    // even in build configurations that wire up no production caller, so the
    // dead-code lint is allowed narrowly here rather than dropping tested code.
    #[allow(dead_code)]
    #[inline]
    pub(crate) fn reset_bookkeeping(&mut self) {
        self.total_in = 0;
        self.total_out = 0;
        self.msg = None;
    }
}

impl<A: Allocator> fmt::Debug for ZStream<A> {
    /// Hand-written because neither the engine state nor an arbitrary allocator
    /// `A` is required to implement [`Debug`]: the state is rendered as its
    /// variant name (see [`StreamState`]'s `Debug`) and the allocator as its
    /// type name.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ZStream")
            .field("total_in", &self.total_in)
            .field("total_out", &self.total_out)
            .field("data_type", &self.data_type)
            .field("adler", &self.adler)
            .field("msg", &self.msg)
            .field("reserved", &self.reserved)
            .field("state", &self.state)
            .field("allocator", &core::any::type_name::<A>())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn new_has_expected_defaults() {
        let strm = ZStream::new();
        assert_eq!(strm.total_in, 0);
        assert_eq!(strm.total_out, 0);
        // `Z_BINARY` == 0, the zeroed default.
        assert_eq!(strm.data_type, 0);
        // Adler-32 of the empty input.
        assert_eq!(strm.adler, 1);
        assert_eq!(strm.msg, None);
        assert_eq!(strm.reserved, 0);
        // No engine yet, and neither direction reports a live state.
        assert!(!strm.has_state());
        assert!(!strm.is_deflate());
        assert!(!strm.is_inflate());
        assert!(strm.deflate_state().is_none());
        assert!(strm.inflate_state().is_none());
    }

    #[test]
    fn default_matches_new() {
        let strm = ZStream::default();
        assert_eq!(strm.total_in, 0);
        assert_eq!(strm.total_out, 0);
        assert_eq!(strm.adler, 1);
        assert!(!strm.has_state());
    }

    #[test]
    fn default_allocator_is_used_by_default() {
        // The `ZStream::new()` path must select `DefaultAllocator`.
        let strm: ZStream = ZStream::new();
        let _alloc: &DefaultAllocator = strm.allocator();
    }

    #[test]
    fn default_allocator_allocates_zeroed_buffers() {
        let alloc = DefaultAllocator;

        // The global-allocator default is infallible, so `allocate_zeroed`
        // always returns `Some`.
        let bytes: AllocBuffer<u8> = alloc.allocate_zeroed(4).expect("infallible");
        assert_eq!(&bytes[..], &[0u8; 4][..]);
        assert_eq!(bytes.len(), 4);

        let words: AllocBuffer<u16> = alloc.allocate_zeroed(3).expect("infallible");
        assert_eq!(&words[..], &[0u16; 3][..]);

        // A zero-length request yields an empty buffer.
        let empty: AllocBuffer<u32> = alloc.allocate_zeroed(0).expect("infallible");
        assert!(empty.is_empty());

        // `deallocate` consumes the buffer (default path just drops it).
        alloc.deallocate(bytes);
        alloc.deallocate(words);
        alloc.deallocate(empty);
    }

    #[test]
    fn install_and_take_inflate_state() {
        let mut strm = ZStream::new();

        // A raw-deflate (wrap = 0) inflate state with a 32 KiB window.
        strm.set_inflate_state(InflateState::new(0, 15));

        assert!(strm.has_state());
        assert!(strm.is_inflate());
        assert!(!strm.is_deflate());
        assert!(strm.inflate_state().is_some());
        // The deflate accessor must not see the inflate state.
        assert!(strm.deflate_state().is_none());

        // Mutable access resolves to the same live state.
        assert!(strm.inflate_state_mut().is_some());

        // Taking the state hands back the `Inflate` variant and empties the
        // stream; the returned box drops here, freeing the state (RAII stands in
        // for `inflateEnd`).
        let taken = strm.take_state();
        assert!(taken.is_inflate());
        assert!(!strm.has_state());
        assert!(strm.inflate_state().is_none());
    }

    #[test]
    fn clear_state_returns_to_none() {
        let mut strm = ZStream::new();
        strm.set_inflate_state(InflateState::new(1, 15));
        assert!(strm.has_state());

        strm.clear_state();
        assert!(!strm.has_state());
        assert!(strm.inflate_state().is_none());
    }

    #[test]
    fn set_inflate_state_replaces_previous() {
        // Installing a second engine drops the first without any explicit
        // teardown — the RAII replacement for a forgotten `inflateEnd`.
        let mut strm = ZStream::new();
        strm.set_inflate_state(InflateState::new(0, 15));
        strm.set_inflate_state(InflateState::new(2, 15));
        assert!(strm.is_inflate());
    }

    #[test]
    fn set_msg_maps_return_codes() {
        let mut strm = ZStream::new();

        strm.set_msg(ReturnCode::StreamError);
        assert_eq!(strm.msg, Some("stream error"));

        strm.set_msg(ReturnCode::DataError);
        assert_eq!(strm.msg, Some("data error"));

        // `Z_OK` has an empty message, which clears the field (C `NULL`).
        strm.set_msg(ReturnCode::Ok);
        assert_eq!(strm.msg, None);
    }

    #[test]
    fn set_msg_str_and_clear() {
        let mut strm = ZStream::new();
        strm.set_msg_str("custom diagnostic");
        assert_eq!(strm.msg, Some("custom diagnostic"));

        strm.clear_msg();
        assert_eq!(strm.msg, None);
    }

    #[test]
    fn data_type_enum_maps_known_and_unknown() {
        let mut strm = ZStream::new();
        // Default `data_type` is `Z_BINARY`.
        assert_eq!(strm.data_type_enum(), Some(DataType::Binary));

        strm.data_type = 1;
        assert_eq!(strm.data_type_enum(), Some(DataType::Text));

        // An inflate decode-state code is not a `DataType`.
        strm.data_type = 99;
        assert_eq!(strm.data_type_enum(), None);
    }

    #[test]
    fn reset_bookkeeping_clears_counters_and_msg_only() {
        let mut strm = ZStream::new();
        strm.total_in = 123;
        strm.total_out = 456;
        strm.data_type = 1;
        strm.adler = 0xDEAD_BEEF;
        strm.set_msg(ReturnCode::BufError);

        strm.reset_bookkeeping();

        assert_eq!(strm.total_in, 0);
        assert_eq!(strm.total_out, 0);
        assert_eq!(strm.msg, None);
        // `data_type` and `adler` are engine-owned and left untouched.
        assert_eq!(strm.data_type, 1);
        assert_eq!(strm.adler, 0xDEAD_BEEF);
    }

    /// An allocator that records, via a borrowed counter, that it was dropped.
    /// Used to prove the `ZStream` drop glue tears down the owned allocator
    /// (and, by the same mechanism, the boxed engine state).
    struct CountingAllocator<'a> {
        drops: &'a AtomicUsize,
    }

    impl Allocator for CountingAllocator<'_> {
        fn allocate_zeroed<T>(&self, count: usize) -> Option<AllocBuffer<T>>
        where
            T: Copy + Default + 'static,
        {
            Some(AllocBuffer::Owned(alloc::vec![T::default(); count]))
        }
    }

    impl Drop for CountingAllocator<'_> {
        fn drop(&mut self) {
            self.drops.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn drop_runs_and_releases_owned_allocator() {
        let drops = AtomicUsize::new(0);
        {
            let strm = ZStream::with_allocator(CountingAllocator { drops: &drops });
            assert!(!strm.has_state());
            assert_eq!(drops.load(Ordering::SeqCst), 0);
        }
        // Leaving the scope drops the `ZStream`, which drops its `alloc` field.
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn custom_allocator_allocates_zeroed() {
        let drops = AtomicUsize::new(0);
        let alloc = CountingAllocator { drops: &drops };
        let buf: AllocBuffer<u8> = alloc.allocate_zeroed(8).expect("infallible");
        assert_eq!(&buf[..], &[0u8; 8][..]);
    }

    #[test]
    fn debug_impl_is_stable_without_debug_bounds() {
        // Exercises the hand-written `Debug` (state has no `Debug`, `A` is not
        // bound to `Debug`).
        let strm = ZStream::new();
        let rendered = alloc::format!("{strm:?}");
        assert!(rendered.contains("ZStream"));
        assert!(rendered.contains("total_in"));
        // The `None` engine variant is printed by name.
        assert!(rendered.contains("None"));
    }

    #[test]
    fn stream_state_debug_prints_variant_name() {
        assert_eq!(alloc::format!("{:?}", StreamState::None), "None");
        let inflate = StreamState::Inflate(InflateState::new(0, 15));
        assert_eq!(alloc::format!("{inflate:?}"), "Inflate");
    }
}
