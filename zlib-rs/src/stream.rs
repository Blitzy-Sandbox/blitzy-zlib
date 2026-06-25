//! Idiomatic streaming object replacing C `z_stream`; owns state via `Box`
//! (RAII).
//!
//! This module defines [`ZStream`], the safe-Rust translation of the C
//! `z_stream` structure (`zlib.h` `struct z_stream_s`, lines 90-110) together
//! with the [`Allocator`] custom-allocation extension point that mirrors the C
//! `zalloc` / `zfree` callbacks (`zlib.h` lines 85-86). It is the central
//! streaming contract of the safe core: every compression and decompression
//! call threads its progress through a `ZStream`.
//!
//! # Memory-ownership re-architecture (AAP §0.3.2, §0.6.3)
//!
//! The C `z_stream` manages memory by hand. It carries an opaque
//! `internal_state *state` pointer that `deflateInit2_` / `inflateInit2_`
//! allocate with the `zalloc` callback and `deflateEnd` / `inflateEnd` release
//! with `zfree`, and it exposes the working buffers as raw `next_in` /
//! `next_out` pointers paired with `avail_in` / `avail_out` counts. That design
//! makes memory leaks and double-frees representable.
//!
//! `ZStream` replaces it with Rust ownership:
//!
//! | C `z_stream` field            | `ZStream` modeling                                   |
//! |-------------------------------|------------------------------------------------------|
//! | `next_in` + `avail_in`        | a borrowed `&[u8]` passed to the engine step methods |
//! | `next_out` + `avail_out`      | a borrowed `&mut [u8]` passed to the step methods    |
//! | `total_in`                    | [`total_in`](ZStream::total_in) (`u64`)              |
//! | `total_out`                   | [`total_out`](ZStream::total_out) (`u64`)            |
//! | `msg`                         | [`msg`](ZStream::msg) (`Option<&'static str>`) plus `Result` returns |
//! | `state` (`internal_state *`)  | a private, owned [`StreamState`] enum (`Box<…State>`)|
//! | `zalloc` / `zfree` / `opaque` | an optional [`Allocator`] handle                     |
//! | `data_type`                   | [`data_type`](ZStream::data_type) (`i32`)            |
//! | `adler`                       | [`adler`](ZStream::adler) (`u32`)                    |
//! | `reserved`                    | intentionally dropped (unused in safe Rust)          |
//!
//! Because the engine state lives in a `Box` owned by the `ZStream`, dropping
//! the stream automatically frees every buffer — the work `deflateEnd` /
//! `inflateEnd` perform in C — so leaks and double-frees are *unrepresentable*
//! rather than merely avoided. Input and output are never owned; they are
//! borrowed slices supplied per call, mirroring the C `next_in` / `next_out`
//! discipline without any raw pointer.
//!
//! # Safety and portability
//!
//! This module is part of the `#![forbid(unsafe_code)]` core: it contains no
//! `unsafe`, no raw pointers, and no `std::` references. It uses only `core`
//! and `alloc`, so the crate's `no_std` build path (mirroring the C `Z_SOLO`
//! configuration) is preserved. The `#[repr(C)]`, raw-pointer-bearing,
//! `zalloc`/`zfree`-function-pointer ABI mirror of `z_stream` lives **only** in
//! the `libz-rs-sys` FFI shim, which is the sole place `unsafe` is permitted.

use alloc::boxed::Box;
use alloc::vec::Vec;

use crate::constants::DataType;
use crate::deflate::state::DeflateState;
use crate::error::{Result, ReturnCode, ZlibError};
use crate::inflate::state::InflateState;

// ===========================================================================
// Allocator extension point
// ===========================================================================

/// Safe-core counterpart of the C `zalloc` / `zfree` allocation callbacks.
///
/// The C `z_stream` lets a caller override allocation through two raw
/// function pointers — `alloc_func zalloc(opaque, items, size)` and
/// `free_func zfree(opaque, address)` (`zlib.h` lines 85-86) — whose defaults
/// are `zcalloc` / `zcfree` (`zutil.h`). Those raw-pointer callbacks cannot
/// exist under `#![forbid(unsafe_code)]`, so this trait re-expresses the same
/// extension point safely: an implementor *vends owned buffers* instead of
/// returning raw addresses.
///
/// # Relationship to the C ABI
///
/// The raw `zalloc` / `zfree` function-pointer ABI is bridged in the
/// `libz-rs-sys` FFI shim (which owns all `unsafe`); a C caller that installs
/// custom callbacks is serviced there. This trait is the **safe-core
/// extension point only** — it is what idiomatic Rust callers and the shim's
/// safe adapter use to influence how the core obtains scratch memory.
///
/// Both methods have default implementations backed by the global allocator,
/// so the common case ([`DefaultAllocator`]) needs no code at all.
pub trait Allocator {
    /// Allocate a zero-initialized byte buffer of exactly `len` bytes.
    ///
    /// The default implementation returns a freshly allocated, zeroed
    /// [`Vec<u8>`]. Zeroing is a safe superset of the C `zcalloc` behavior
    /// (which uses `malloc` and does not zero): callers that relied on
    /// uninitialized memory still observe valid, in-bounds bytes.
    #[must_use]
    fn alloc_bytes(&self, len: usize) -> Vec<u8> {
        alloc::vec![0u8; len]
    }

    /// Release a buffer previously produced by [`alloc_bytes`](Allocator::alloc_bytes).
    ///
    /// Taking the buffer *by value* is the safe analogue of passing an address
    /// to the C `zfree` callback: ownership transfers in, and the default
    /// implementation drops it, letting Rust free the backing storage. A
    /// custom allocator that pools memory can override this to recycle the
    /// buffer instead.
    fn free_bytes(&self, buffer: Vec<u8>) {
        drop(buffer);
    }
}

/// The default [`Allocator`]: a zero-sized handle over the global allocator.
///
/// This mirrors the C library's default `zcalloc` / `zcfree` behavior without
/// any `unsafe`. It carries no state, so it is `Copy` and free to construct.
#[derive(Debug, Default, Clone, Copy)]
pub struct DefaultAllocator;

impl Allocator for DefaultAllocator {}

// ===========================================================================
// Owned engine state
// ===========================================================================

/// The owned compressor/decompressor state held by a [`ZStream`].
///
/// This is the safe replacement for the C `internal_state *state` pointer. A
/// single `z_stream` is shared by both directions in C; here the owned state
/// is an enum so the type system records which engine (if any) is active and
/// guarantees the matching [`Box`] is the *sole* owner of the heap state.
///
/// The enum is deliberately private: callers interact with it only through the
/// typed accessors on [`ZStream`] (which never leak the variant), so the safe
/// core retains full control over state transitions. [`Default`] yields
/// [`StreamState::None`], which lets [`core::mem::take`] cheaply extract and
/// drop the active state during teardown.
#[derive(Default)]
enum StreamState {
    /// No engine is initialized — the stream has been freshly constructed or
    /// has been torn down.
    #[default]
    None,
    /// An initialized DEFLATE compressor, owned via [`Box`].
    Deflate(Box<DeflateState>),
    /// An initialized inflate decompressor, owned via [`Box`].
    Inflate(Box<InflateState>),
}

impl StreamState {
    /// A short, human-readable discriminant tag used by [`ZStream`]'s
    /// [`Debug`](core::fmt::Debug) implementation (which cannot recurse into
    /// the engine state because [`DeflateState`] is not itself `Debug`).
    const fn kind_str(&self) -> &'static str {
        match self {
            StreamState::None => "none",
            StreamState::Deflate(_) => "deflate",
            StreamState::Inflate(_) => "inflate",
        }
    }
}

// ===========================================================================
// ZStream
// ===========================================================================

/// The idiomatic streaming object that replaces the C `z_stream`.
///
/// A `ZStream` carries the stream-level bookkeeping shared by compression and
/// decompression and *owns* the active engine state (see [`StreamState`]). The
/// working input/output buffers are **not** stored on the struct: they are
/// passed as borrowed slices to the engine step methods provided by the
/// `crate::deflate` and `crate::inflate` modules, which is the safe analogue of
/// the C `next_in` / `next_out` pointer pair.
///
/// Construction with [`ZStream::new`] produces an *uninitialized* stream
/// ([`StreamState::None`]); the `crate::deflate` / `crate::inflate` modules
/// install the engine via [`set_deflate_state`](ZStream::set_deflate_state) /
/// [`set_inflate_state`](ZStream::set_inflate_state) as part of their
/// `deflateInit2` / `inflateInit2` equivalents.
///
/// # RAII teardown
///
/// Dropping a `ZStream` drops the owned [`Box`] (and therefore every buffer the
/// engine allocated), performing the cleanup that C delegates to `deflateEnd` /
/// `inflateEnd`. No manual `Drop` implementation is required or provided; the
/// automatic `Drop` of `Box` / `Vec` makes leaks and double-frees
/// unrepresentable. The optional [`end`](ZStream::end) method exists only to
/// surface the C return-code semantics (ending an uninitialized stream is a
/// `Z_STREAM_ERROR`); it does no `unsafe` work.
pub struct ZStream {
    /// Total number of input bytes consumed so far (C `uLong total_in`).
    pub total_in: u64,
    /// Total number of output bytes produced so far (C `uLong total_out`).
    pub total_out: u64,
    /// Best guess about the data type, or the inflate decoding state hint
    /// (C `int data_type`). Carries [`crate::constants::DataType`] semantics;
    /// use [`data_type`](ZStream::data_type) for a typed view.
    pub data_type: i32,
    /// Running Adler-32 or CRC-32 checksum of the uncompressed data
    /// (C `uLong adler`).
    pub adler: u32,
    /// Last informational message, or `None` when there is none (C `char *msg`,
    /// `NULL` if no error). Kept as a `&'static str` mirror for FFI parity; the
    /// authoritative error channel in the safe core is the `Result` return.
    pub msg: Option<&'static str>,
    /// The owned engine state. Private so the safe core controls all state
    /// transitions through the typed accessors below.
    state: StreamState,
    /// Optional custom allocator handle (the safe analogue of `zalloc` /
    /// `zfree` / `opaque`). `None` selects the global allocator.
    allocator: Option<Box<dyn Allocator>>,
}

impl ZStream {
    /// Create a fresh, uninitialized stream.
    ///
    /// The returned stream has no engine state ([`StreamState::None`]), zeroed
    /// counters, no message, no custom allocator, and a `data_type` of
    /// [`DataType::Unknown`] — matching the value a `z_stream` carries after
    /// `deflateReset` / `inflateReset`. The engine is installed later by the
    /// `crate::deflate` / `crate::inflate` initialization routines.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            total_in: 0,
            total_out: 0,
            data_type: DataType::Unknown.as_i32(),
            adler: 0,
            msg: None,
            state: StreamState::None,
            allocator: None,
        }
    }

    /// Returns `true` if an engine (deflate or inflate) is currently installed.
    #[must_use]
    pub const fn is_initialized(&self) -> bool {
        !matches!(self.state, StreamState::None)
    }

    /// Returns `true` if this stream currently owns a DEFLATE compressor.
    #[must_use]
    pub const fn is_deflate(&self) -> bool {
        matches!(self.state, StreamState::Deflate(_))
    }

    /// Returns `true` if this stream currently owns an inflate decompressor.
    #[must_use]
    pub const fn is_inflate(&self) -> bool {
        matches!(self.state, StreamState::Inflate(_))
    }

    /// Install a DEFLATE compressor, taking ownership of its boxed state.
    ///
    /// Any previously installed engine is dropped (freeing its buffers). This
    /// is the hook the `crate::deflate` `deflateInit2` equivalent uses after it
    /// has validated parameters and built the [`DeflateState`].
    pub fn set_deflate_state(&mut self, state: Box<DeflateState>) {
        self.state = StreamState::Deflate(state);
    }

    /// Install an inflate decompressor, taking ownership of its boxed state.
    ///
    /// Any previously installed engine is dropped (freeing its buffers). This
    /// is the hook the `crate::inflate` `inflateInit2` equivalent uses.
    pub fn set_inflate_state(&mut self, state: Box<InflateState>) {
        self.state = StreamState::Inflate(state);
    }

    /// Borrow the DEFLATE compressor state, or `None` if this is not a
    /// (currently initialized) deflate stream.
    #[must_use]
    pub fn deflate_state(&self) -> Option<&DeflateState> {
        match &self.state {
            StreamState::Deflate(state) => Some(state),
            _ => None,
        }
    }

    /// Mutably borrow the DEFLATE compressor state, or `None` if this is not a
    /// (currently initialized) deflate stream. The compressor driver in
    /// `crate::deflate` uses this to advance the engine.
    pub fn deflate_state_mut(&mut self) -> Option<&mut DeflateState> {
        match &mut self.state {
            StreamState::Deflate(state) => Some(state),
            _ => None,
        }
    }

    /// Borrow the inflate decompressor state, or `None` if this is not a
    /// (currently initialized) inflate stream.
    #[must_use]
    pub fn inflate_state(&self) -> Option<&InflateState> {
        match &self.state {
            StreamState::Inflate(state) => Some(state),
            _ => None,
        }
    }

    /// Mutably borrow the inflate decompressor state, or `None` if this is not
    /// a (currently initialized) inflate stream. The decompressor driver in
    /// `crate::inflate` uses this to advance the engine.
    pub fn inflate_state_mut(&mut self) -> Option<&mut InflateState> {
        match &mut self.state {
            StreamState::Inflate(state) => Some(state),
            _ => None,
        }
    }

    /// Mutably borrow the DEFLATE state, or return [`ZlibError::StreamError`].
    ///
    /// This is the ergonomic form the deflate driver uses at entry, mirroring
    /// the C habit of returning `Z_STREAM_ERROR` when `strm->state` is the
    /// wrong kind or `Z_NULL`.
    pub fn deflate_state_or_err(&mut self) -> Result<&mut DeflateState> {
        self.deflate_state_mut().ok_or(ZlibError::StreamError)
    }

    /// Mutably borrow the inflate state, or return [`ZlibError::StreamError`].
    ///
    /// The inflate-direction analogue of
    /// [`deflate_state_or_err`](ZStream::deflate_state_or_err).
    pub fn inflate_state_or_err(&mut self) -> Result<&mut InflateState> {
        self.inflate_state_mut().ok_or(ZlibError::StreamError)
    }

    /// Tear down the installed engine, returning the C-compatible status code.
    ///
    /// This realizes the `deflateEnd` / `inflateEnd` contract: the owned engine
    /// state (and all its buffers) is dropped, and the stream returns to the
    /// uninitialized [`StreamState::None`]. Ending a stream that has no engine
    /// installed is a [`ZlibError::StreamError`] (the C `Z_STREAM_ERROR`),
    /// matching how `deflateEnd` rejects a `Z_NULL` state. On success it yields
    /// [`ReturnCode::Ok`].
    ///
    /// Note that this is *optional*: simply dropping the `ZStream` frees the
    /// same memory. `end` exists to expose the return-code semantics callers
    /// (and the FFI shim) may need.
    pub fn end(&mut self) -> Result<ReturnCode> {
        match core::mem::take(&mut self.state) {
            StreamState::None => Err(ZlibError::StreamError),
            _ => Ok(ReturnCode::Ok),
        }
    }

    /// Reset the stream-level bookkeeping cleared by `deflateReset` /
    /// `inflateReset`.
    ///
    /// Zeroes [`total_in`](ZStream::total_in) and
    /// [`total_out`](ZStream::total_out), clears [`msg`](ZStream::msg), and
    /// resets [`data_type`](ZStream::data_type) to [`DataType::Unknown`]. This
    /// is a *plumbing hook*: the algorithmic engine reset (re-initializing the
    /// window, trees, and bit buffer) lives in the `crate::deflate` /
    /// `crate::inflate` modules, which call this for the shared scalars. The
    /// running [`adler`](ZStream::adler) checksum is intentionally left
    /// untouched here because its reset value depends on the wrapper format
    /// (zlib vs raw vs gzip) and is set by the engine.
    pub fn reset_counters(&mut self) {
        self.total_in = 0;
        self.total_out = 0;
        self.msg = None;
        self.data_type = DataType::Unknown.as_i32();
    }

    /// Interpret [`data_type`](ZStream::data_type) as a typed
    /// [`DataType`], or `None` if the raw value is outside the documented
    /// range. The raw `i32` field remains authoritative for FFI parity.
    #[must_use]
    pub const fn data_type(&self) -> Option<DataType> {
        DataType::try_from_i32(self.data_type)
    }

    /// Install a custom [`Allocator`] handle (the safe analogue of setting the
    /// C `zalloc` / `zfree` / `opaque` triple). Replaces any previous handle.
    pub fn set_allocator(&mut self, allocator: Box<dyn Allocator>) {
        self.allocator = Some(allocator);
    }

    /// Borrow the installed custom [`Allocator`], or `None` when the global
    /// allocator (the [`DefaultAllocator`] behavior) is in effect.
    #[must_use]
    pub fn allocator(&self) -> Option<&dyn Allocator> {
        self.allocator.as_deref()
    }
}

impl Default for ZStream {
    /// Equivalent to [`ZStream::new`]: a fresh, uninitialized stream.
    fn default() -> Self {
        Self::new()
    }
}

impl core::fmt::Debug for ZStream {
    /// Hand-written because the owned [`DeflateState`] is not itself `Debug`.
    /// The engine state is summarized by its kind tag rather than dumped, and
    /// the allocator is reported only as present/absent.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ZStream")
            .field("total_in", &self.total_in)
            .field("total_out", &self.total_out)
            .field("data_type", &self.data_type)
            .field("adler", &self.adler)
            .field("msg", &self.msg)
            .field("state", &self.state.kind_str())
            .field("has_custom_allocator", &self.allocator.is_some())
            .finish()
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::constants::{DEF_MEM_LEVEL, Strategy, Z_DEFLATED};

    /// A freshly constructed stream is empty and reports zeroed bookkeeping.
    #[test]
    fn new_is_empty() {
        let z = ZStream::new();
        assert!(matches!(z.state, StreamState::None));
        assert!(!z.is_initialized());
        assert!(!z.is_deflate());
        assert!(!z.is_inflate());
        assert_eq!(z.total_in, 0);
        assert_eq!(z.total_out, 0);
        assert_eq!(z.adler, 0);
        assert_eq!(z.msg, None);
        // Fresh streams report the "unknown" data-type hint.
        assert_eq!(z.data_type(), Some(DataType::Unknown));
        // No custom allocator is installed by default.
        assert!(z.allocator().is_none());
    }

    /// `Default` is equivalent to `new`.
    #[test]
    fn default_equals_new() {
        let z = ZStream::default();
        assert!(matches!(z.state, StreamState::None));
        assert!(!z.is_initialized());
        assert_eq!(z.total_in, 0);
        assert_eq!(z.total_out, 0);
        assert_eq!(z.data_type, ZStream::new().data_type);
    }

    /// Ending a stream that was never initialized is a stream error.
    #[test]
    fn end_on_empty_is_stream_error() {
        let mut z = ZStream::new();
        assert_eq!(z.end(), Err(ZlibError::StreamError));
    }

    /// Installing a real DEFLATE engine, observing it through the accessors,
    /// and ending it returns to the empty state (and frees the owned buffers).
    #[test]
    fn install_and_end_deflate_state() {
        let mut z = ZStream::new();
        // The real `DeflateState::new` signature from `deflate/state.rs`.
        let state = DeflateState::new(6, Z_DEFLATED as u8, 15, DEF_MEM_LEVEL, Strategy::Default, 1)
            .expect("deflate state allocation should succeed");
        z.set_deflate_state(Box::new(state));

        assert!(z.is_initialized());
        assert!(z.is_deflate());
        assert!(!z.is_inflate());
        assert!(z.deflate_state().is_some());
        assert!(z.deflate_state_mut().is_some());
        assert!(z.inflate_state().is_none());
        assert!(z.inflate_state_mut().is_none());
        assert!(z.deflate_state_or_err().is_ok());
        assert_eq!(z.inflate_state_or_err().err(), Some(ZlibError::StreamError));

        // Teardown drops the owned `Box` (RAII) and returns to `None`.
        assert_eq!(z.end(), Ok(ReturnCode::Ok));
        assert!(!z.is_initialized());
        assert!(matches!(z.state, StreamState::None));
        // A second teardown now reports the stream error.
        assert_eq!(z.end(), Err(ZlibError::StreamError));
    }

    /// Installing a second engine drops the first (no leak, no double free —
    /// guaranteed by ownership), then teardown succeeds.
    #[test]
    fn reinstalling_state_replaces_previous() {
        let mut z = ZStream::new();
        let first = DeflateState::new(1, Z_DEFLATED as u8, 9, DEF_MEM_LEVEL, Strategy::Default, 0)
            .expect("first allocation");
        z.set_deflate_state(Box::new(first));
        let second =
            DeflateState::new(9, Z_DEFLATED as u8, 15, DEF_MEM_LEVEL, Strategy::Default, 2)
                .expect("second allocation");
        z.set_deflate_state(Box::new(second));
        assert!(z.is_deflate());
        // The replacement is observable: wrap == 2 (gzip) on the live state.
        assert_eq!(z.deflate_state().expect("present").wrap, 2);
        assert_eq!(z.end(), Ok(ReturnCode::Ok));
    }

    /// `reset_counters` clears the shared bookkeeping but not the engine.
    #[test]
    fn reset_counters_clears_bookkeeping() {
        let mut z = ZStream::new();
        z.total_in = 123;
        z.total_out = 456;
        z.msg = Some("boom");
        z.data_type = DataType::Binary.as_i32();
        z.adler = 0xDEAD_BEEF;

        z.reset_counters();

        assert_eq!(z.total_in, 0);
        assert_eq!(z.total_out, 0);
        assert_eq!(z.msg, None);
        assert_eq!(z.data_type(), Some(DataType::Unknown));
        // `adler` is intentionally left for the engine to reset.
        assert_eq!(z.adler, 0xDEAD_BEEF);
    }

    /// The default allocator vends zero-initialized buffers of the requested
    /// length, and accepts them back.
    #[test]
    fn default_allocator_vends_zeroed_buffers() {
        let allocator = DefaultAllocator;
        let buffer = allocator.alloc_bytes(8);
        assert_eq!(buffer.len(), 8);
        assert!(buffer.iter().all(|&b| b == 0));
        // Zero-length allocation is valid and yields an empty buffer.
        assert_eq!(allocator.alloc_bytes(0).len(), 0);
        allocator.free_bytes(buffer);
    }

    /// A custom allocator handle round-trips through the stream and is usable.
    #[test]
    fn custom_allocator_handle_round_trips() {
        let mut z = ZStream::new();
        assert!(z.allocator().is_none());
        z.set_allocator(Box::new(DefaultAllocator));
        let allocator = z.allocator().expect("allocator should be installed");
        let buffer = allocator.alloc_bytes(4);
        assert_eq!(buffer.len(), 4);
        assert!(buffer.iter().all(|&b| b == 0));
    }

    /// The typed `data_type` accessor maps raw values and rejects out-of-range.
    #[test]
    fn data_type_accessor_maps_raw_value() {
        let mut z = ZStream::new();
        z.data_type = DataType::Text.as_i32();
        assert_eq!(z.data_type(), Some(DataType::Text));
        z.data_type = DataType::Binary.as_i32();
        assert_eq!(z.data_type(), Some(DataType::Binary));
        z.data_type = 999;
        assert_eq!(z.data_type(), None);
    }

    /// The hand-written `Debug` impl summarizes the stream without recursing
    /// into the (non-`Debug`) engine state.
    #[test]
    fn debug_summarizes_without_engine_recursion() {
        let z = ZStream::new();
        let rendered = alloc::format!("{z:?}");
        assert!(rendered.contains("ZStream"));
        assert!(rendered.contains("state"));
        assert!(rendered.contains("none"));
        assert!(rendered.contains("has_custom_allocator"));
    }
}
