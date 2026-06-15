//! Internal inflate state — the safe-Rust port of C `inflate.h`.
//!
//! This module defines the two artifacts that the entire decompression engine
//! is built around:
//!
//! * [`InflateMode`] — the decode-state enum (the C `inflate_mode` typedef), an
//!   exhaustive set of 32 states that the [`inflate`](crate::inflate) driver
//!   matches on as it walks a DEFLATE / zlib / gzip stream; and
//! * [`InflateState`] — the heap-resident state struct (the C
//!   `struct inflate_state`) that is carried between `inflate()` calls. It holds
//!   the sliding window, the bit accumulator, the dynamically built Huffman
//!   decode tables, and all of the bookkeeping needed to suspend and resume
//!   decoding at any byte boundary.
//!
//! # Ownership model (AAP §0.6.2 / §0.6.3)
//!
//! In C, the `z_stream` carries an opaque `void *state` that points at a
//! `struct inflate_state` allocated with `ZALLOC` and released by
//! `inflateEnd`/`ZFREE`. Here that relationship is **inverted and made safe**:
//! [`crate::stream::ZStream`] *owns* the state as an
//! `Option<Box<dyn StreamState>>`, so:
//!
//! * the raw `void *state` becomes an owned [`Box`] freed deterministically on
//!   [`Drop`] (the idiomatic replacement for `inflateEnd`);
//! * the raw `unsigned char *window` becomes an owned [`Vec<u8>`];
//! * the fixed `code codes[ENOUGH]` array becomes an owned `Box<[Code]>`; and
//! * the C `code *` cursors (`lencode`, `distcode`, `next`) become **`usize`
//!   indices** into that `codes` buffer (the central pointer→index
//!   transformation — see the field docs).
//!
//! Because there are no raw pointers, double-free and use-after-free are
//! *unrepresentable*, and the struct is freely [`Clone`]-able (needed by
//! `inflateCopy`): cloning copies the indices verbatim, and they remain valid
//! against the cloned `codes`/`window` buffers with no pointer re-basing.
//!
//! The C `z_streamp strm` back-pointer is **intentionally omitted**: the stream
//! owns the state, not the other way around, so a back-pointer would be a
//! self-reference. Engine functions instead receive the stream's input/output
//! slices as parameters.
//!
//! # State-transition diagram (from C `inflate.h`)
//!
//! Most modes can additionally transition to [`Bad`](InflateMode::Bad) or
//! [`Mem`](InflateMode::Mem) on error (omitted below for clarity):
//!
//! ```text
//! Process header:
//!     HEAD -> (gzip) or (zlib) or (raw)
//!     (gzip) -> FLAGS -> TIME -> OS -> EXLEN -> EXTRA -> NAME -> COMMENT ->
//!               HCRC -> TYPE
//!     (zlib) -> DICTID or TYPE
//!     DICTID -> DICT -> TYPE
//!     (raw) -> TYPEDO
//! Read deflate blocks:
//!     TYPE -> TYPEDO -> STORED or TABLE or LEN_ or CHECK
//!     STORED -> COPY_ -> COPY -> TYPE
//!     TABLE -> LENLENS -> CODELENS -> LEN_
//!     LEN_ -> LEN
//! Read deflate codes in fixed or dynamic block:
//!     LEN -> LENEXT or LIT or TYPE
//!     LENEXT -> DIST -> DISTEXT -> MATCH -> LEN
//!     LIT -> LEN
//! Process trailer:
//!     CHECK -> LENGTH -> DONE
//! ```
//!
//! # Safety
//!
//! This module is **100% safe Rust** — it contains no `unsafe` blocks (AAP
//! §0.7.2). It is also `no_std`-clean: it references only [`core`] and
//! [`alloc`] (for [`Box`]/[`Vec`]), so it compiles under
//! `--no-default-features --features no-std`. The [`head`](InflateState::head)
//! field and all gzip-header capture are gated behind the `gzip` feature.

// ===========================================================================
// Imports
// ===========================================================================
//
// Heap container types backing the owned `window`/`codes` buffers. Under the
// default `std` build `Box`/`Vec` are in the prelude; under a `no-std` build
// they come from the `alloc` crate (which `lib.rs` brings into scope with
// `extern crate alloc;`). This mirrors the established pattern in
// `crate::stream`, keeping the source identical across both configurations.
#[cfg(not(feature = "std"))]
use alloc::{boxed::Box, vec::Vec};

// `Any` powers the safe downcast from `dyn StreamState` back to this concrete
// type. It lives in `core`, so it is available in `no_std` builds.
use core::any::Any;

// The canonical decode-table entry type and the maximum table size are owned by
// the table builder in `crate::inflate::tables`; we reuse them rather than
// redefining (`Code` is `#[repr(C)]` and must stay the single source of truth).
use crate::inflate::tables::{Code, ENOUGH};

// `ZStream` owns this state behind the object-safe `StreamState` trait; we
// report `StreamKind::Inflate` and provide the `Any` upcasts so an engine can
// recover the concrete `InflateState`.
use crate::stream::{StreamKind, StreamState};

// Captured gzip header (read side, `inflateGetHeader`). The entire concept is
// gzip-only, so the import — like the field — is gated behind the `gzip`
// feature.
#[cfg(feature = "gzip")]
use crate::gz_header::GzHeader;

// ===========================================================================
// InflateMode — the decode-state machine (C `inflate_mode`)
// ===========================================================================

/// The inflate decode state, enumerating every position at which decoding may
/// be suspended between `inflate()` calls.
///
/// This is the safe-Rust analogue of the C `inflate_mode` typedef in
/// `inflate.h`. The C enum starts at `HEAD = 16180` purely so a zero-filled
/// `z_stream` cannot accidentally land on a valid mode; Rust enums cannot be
/// constructed uninitialized, so this port uses ordinary **zero-based
/// discriminants** (`Head == 0` … `Sync == 31`). The discriminant values are an
/// internal implementation detail — they are **never** serialized to the wire
/// and are **not** part of the C ABI — so renumbering them is safe.
///
/// The variant *order* is preserved exactly from `inflate.h` (lines 20-53) so
/// that the textual correspondence with the upstream source is obvious to
/// anyone cross-referencing the two implementations.
///
/// # C-name cross-reference
///
/// Two upstream modes have names that do not translate directly:
///
/// | C name  | Rust variant            | Why |
/// |---------|-------------------------|-----|
/// | `COPY_` | [`CopyBegin`](Self::CopyBegin) | trailing `_` is unidiomatic; this is the "first time in" entry to a stored-block copy |
/// | `LEN_`  | [`LenBegin`](Self::LenBegin)   | same — the "first time in" entry to code decoding |
///
/// The two `*Begin` variants are deliberately **distinct** from
/// [`Copy`](Self::Copy) / [`Len`](Self::Len): the driver in `crate::inflate`
/// relies on the distinction (for example, the `data_type` report treats the
/// "first time in" states specially by adding `256`).
///
/// `Match` is a valid identifier (only the bare keyword `match` is reserved),
/// so the upstream `MATCH` mode maps to [`Match`](Self::Match) unchanged.
///
/// # Default
///
/// [`Head`](Self::Head) is the [`Default`], matching the value to which every
/// reset (`inflateReset`/`inflateResetKeep`) returns the machine.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum InflateMode {
    /// i: waiting for magic header.
    #[default]
    Head,
    /// i: waiting for method and flags (gzip).
    Flags,
    /// i: waiting for modification time (gzip).
    Time,
    /// i: waiting for extra flags and operating system (gzip).
    Os,
    /// i: waiting for extra length (gzip).
    Exlen,
    /// i: waiting for extra bytes (gzip).
    Extra,
    /// i: waiting for end of file name (gzip).
    Name,
    /// i: waiting for end of comment (gzip).
    Comment,
    /// i: waiting for header CRC (gzip).
    Hcrc,
    /// i: waiting for dictionary check value.
    Dictid,
    /// waiting for `inflateSetDictionary()` call.
    Dict,
    /// i: waiting for type bits, including last-flag bit.
    Type,
    /// i: same as [`Type`](Self::Type), but skip check to exit inflate on a new
    /// block.
    Typedo,
    /// i: waiting for stored size (length and its complement).
    Stored,
    /// i/o: same as [`Copy`](Self::Copy) below, but only the first time in
    /// (the C `COPY_` mode).
    CopyBegin,
    /// i/o: waiting for input or output to copy a stored block.
    Copy,
    /// i: waiting for dynamic block table lengths.
    Table,
    /// i: waiting for code length code lengths.
    Lenlens,
    /// i: waiting for length/literal and distance code lengths.
    Codelens,
    /// i: same as [`Len`](Self::Len) below, but only the first time in
    /// (the C `LEN_` mode).
    LenBegin,
    /// i: waiting for a length/literal/end-of-block code.
    Len,
    /// i: waiting for length extra bits.
    Lenext,
    /// i: waiting for a distance code.
    Dist,
    /// i: waiting for distance extra bits.
    Distext,
    /// o: waiting for output space to copy a string.
    Match,
    /// o: waiting for output space to write a literal.
    Lit,
    /// i: waiting for the 32-bit check value.
    Check,
    /// i: waiting for the 32-bit length (gzip).
    Length,
    /// finished check, done — remain here until reset.
    Done,
    /// got a data error — remain here until reset (maps to `Z_DATA_ERROR`).
    Bad,
    /// got an `inflate()` memory error — remain here until reset.
    Mem,
    /// looking for synchronization bytes to restart `inflate()`.
    Sync,
}

// ===========================================================================
// InflateState — the resumable decode state (C `struct inflate_state`)
// ===========================================================================

/// State maintained between `inflate()` calls — the safe-Rust analogue of the C
/// `struct inflate_state` (`inflate.h`, lines 80-126).
///
/// In C this struct is approximately 7 KB, not counting the separately
/// allocated sliding window (up to 32 KB). Here every C field is mapped
/// one-for-one, with three categories of transformation applied (AAP §0.6.3):
///
/// * **Raw buffers → owned containers.** `unsigned char *window` becomes an
///   owned [`Vec<u8>`]; the inline `code codes[ENOUGH]` array becomes an owned
///   `Box<[Code]>`. Both are freed automatically on [`Drop`].
/// * **`code *` cursors → `usize` indices.** The C `lencode`, `distcode`, and
///   `next` pointers all point *into* `codes[]`; here they are stored as plain
///   indices into [`codes`](Self::codes). This is what makes the struct
///   trivially [`Clone`]-able for `inflateCopy`: indices stay valid after a
///   clone, whereas raw pointers would dangle into the original allocation.
/// * **Back-pointer dropped.** The C `z_streamp strm` field is omitted (see the
///   [module docs](self) — ownership is inverted).
///
/// # Cloning (`inflateCopy`)
///
/// `#[derive(Clone)]` is sound precisely because the state holds **no
/// pointers**: every member is either a value, an owned buffer ([`Vec`]/
/// [`Box`], which deep-copy), or a `usize` index. A clone is therefore a fully
/// independent, immediately usable inflate state — exactly the contract of
/// `inflateCopy`.
#[derive(Clone)]
pub struct InflateState {
    // NOTE: C's `z_streamp strm` back-pointer is intentionally omitted — the
    // ZStream owns this state (`Option<Box<...>>`); engine functions receive
    // the stream's in/out slices as parameters instead (no self-reference).
    /// Current inflate mode (C `inflate_mode mode`).
    pub mode: InflateMode,

    /// `true` if processing the last block of the stream (C `int last`).
    pub last: bool,

    /// Wrapper-format selector (C `int wrap`), a bitmask:
    ///
    /// * bit 0 (`1`) — expect/produce a **zlib** trailer (Adler-32);
    /// * bit 1 (`2`) — **gzip** wrapper;
    /// * bit 2 (`4`) — validate the check value.
    pub wrap: i32,

    /// `true` once a preset dictionary has been provided (C `int havedict`).
    pub havedict: bool,

    /// gzip header method + flags byte; `0` for a zlib stream; `-1` for a raw
    /// stream or before any header has been seen (C `int flags`). Set while
    /// parsing the `HEAD`/`FLAGS` states.
    pub flags: i32,

    /// zlib-header maximum match distance (C `unsigned dmax`, used under
    /// `INFLATE_STRICT`). Defaults to `32768` (= `1 << 15`).
    pub dmax: u32,

    /// Protected copy of the running check value — Adler-32 or CRC-32, both
    /// 32-bit (C `unsigned long check`).
    pub check: u32,

    /// Protected copy of the total number of output bytes produced
    /// (C `unsigned long total`). Widened to [`u64`] for 64-bit-clean totals.
    pub total: u64,

    /// Where to store the parsed gzip header, if the caller requested capture
    /// via `inflateGetHeader` (C `gz_headerp head`).
    ///
    /// `Some` exactly when capture was requested; the gzip header-parse states
    /// fill in its fields. gzip-only, hence `#[cfg(feature = "gzip")]`.
    #[cfg(feature = "gzip")]
    pub head: Option<GzHeader>,

    // --- sliding window ----------------------------------------------------
    /// Log base 2 of the requested window size (C `unsigned wbits`).
    pub wbits: u32,

    /// Window size in bytes, or `0` if the window is not yet in use
    /// (C `unsigned wsize`).
    pub wsize: u32,

    /// Number of valid bytes currently in the window (C `unsigned whave`).
    pub whave: u32,

    /// Window write index — where the next output byte is written
    /// (C `unsigned wnext`).
    pub wnext: u32,

    /// The sliding window itself (C `unsigned char *window`).
    ///
    /// Allocated **lazily** by `updatewindow` (in `crate::inflate`) to
    /// `1 << wbits` bytes the first time a window is needed; it starts **empty**
    /// and an empty `window` means "no window allocated yet".
    pub window: Vec<u8>,

    // --- bit accumulator ---------------------------------------------------
    /// Input bit accumulator (C `unsigned long hold`).
    ///
    /// **Hold width (AAP §0.6.4):** this is a [`u32`], matching C's portable
    /// `unsigned long` path and `inffast.c`'s assumption of refilling two bytes
    /// at a time once fewer than 15 bits remain. The fast loop
    /// (`crate::inflate::fast`) and the drivers (`crate::inflate` /
    /// `crate::inflate::back`) **must** keep the same refill cadence so output
    /// is byte-identical to canonical zlib.
    pub hold: u32,

    /// Number of valid bits currently in [`hold`](Self::hold)
    /// (C `unsigned bits`).
    pub bits: u32,

    // --- string / stored-block copy ----------------------------------------
    /// Literal value, or the length of data to copy (C `unsigned length`).
    pub length: u32,

    /// Distance back from the current output position to copy a string from
    /// (C `unsigned offset`).
    pub offset: u32,

    // --- table / code decoding ---------------------------------------------
    /// Number of extra bits needed for the current code (C `unsigned extra`).
    pub extra: u32,

    /// **Index** into [`codes`](Self::codes) of the root table for
    /// length/literal codes (C `code *lencode`, a pointer into `codes[]`).
    pub lencode: usize,

    /// **Index** into [`codes`](Self::codes) of the root table for distance
    /// codes (C `code *distcode`, a pointer into `codes[]`).
    pub distcode: usize,

    /// Number of root index bits for [`lencode`](Self::lencode)
    /// (C `unsigned lenbits`).
    pub lenbits: u32,

    /// Number of root index bits for [`distcode`](Self::distcode)
    /// (C `unsigned distbits`).
    pub distbits: u32,

    // --- dynamic table building --------------------------------------------
    /// Number of code-length code lengths (C `unsigned ncode`).
    pub ncode: u32,

    /// Number of length code lengths (C `unsigned nlen`).
    pub nlen: u32,

    /// Number of distance code lengths (C `unsigned ndist`).
    pub ndist: u32,

    /// Number of code lengths accumulated so far in [`lens`](Self::lens)
    /// (C `unsigned have`).
    pub have: u32,

    /// **Index** of the next available slot in [`codes`](Self::codes)
    /// (C `code *next`, a cursor into `codes[]`). Advanced by the table builder
    /// as it consumes entries.
    pub next: usize,

    /// Temporary storage for code lengths while building tables
    /// (C `unsigned short lens[320]`).
    pub lens: [u16; 320],

    /// Scratch work area used while building code tables
    /// (C `unsigned short work[288]`).
    pub work: [u16; 288],

    /// Backing storage for the constructed decode tables
    /// (C `code codes[ENOUGH]`). Exactly [`ENOUGH`] entries long; the
    /// `lencode`/`distcode`/`next` indices address slots within it.
    pub codes: Box<[Code]>,

    /// If `false`, allow an invalid "distance too far back" rather than
    /// rejecting it (C `int sane`). Defaults to `true`.
    pub sane: bool,

    /// Bits back of the last unprocessed length/literal, used by
    /// `inflateMark`; `-1` means "no last length yet" (C `int back`).
    pub back: i32,

    /// Initial length of the current match (C `unsigned was`).
    pub was: u32,
}

// ===========================================================================
// Construction & reset helpers
// ===========================================================================

/// Default zlib-header maximum match distance, `1 << 15` = 32768 bytes.
///
/// Mirrors the `state->dmax = 32768U` assignment in C `inflateResetKeep`
/// (`inflate.c`). Kept as a private named constant so the constructor and the
/// reset helper cannot drift apart.
const DEFAULT_DMAX: u32 = 1 << 15;

impl InflateState {
    /// Creates a fresh inflate state with all buffers allocated and every field
    /// in its post-`inflateInit`/`inflateResetKeep` value.
    ///
    /// Concretely this:
    ///
    /// * allocates [`codes`](Self::codes) as a `Box<[Code]>` of exactly
    ///   [`ENOUGH`] (`1444`) default entries;
    /// * leaves [`window`](Self::window) **empty** — it is allocated lazily by
    ///   `updatewindow` in `crate::inflate` once a window is actually needed;
    /// * zeroes the [`lens`](Self::lens)/[`work`](Self::work) scratch arrays;
    /// * sets [`mode`](Self::mode) to [`InflateMode::Head`],
    ///   [`sane`](Self::sane) to `true`, [`back`](Self::back) to `-1`,
    ///   [`flags`](Self::flags) to `-1`, and [`dmax`](Self::dmax) to `32768`.
    ///
    /// The heavier reconfiguration performed by `inflateReset2`
    /// (deriving [`wbits`](Self::wbits)/[`wrap`](Self::wrap) from `windowBits`)
    /// is intentionally **not** done here; it lives in `crate::inflate`, which
    /// calls [`new`](Self::new) and then layers the window/wrapper setup on top.
    #[must_use]
    pub fn new() -> Self {
        // Allocate `codes` without the `vec!` macro so the source is identical
        // under `std` and `no-std` (matching the `Vec::new()` + `resize()` +
        // `into_boxed_slice()` idiom used in `crate::stream`).
        let mut codes: Vec<Code> = Vec::new();
        codes.resize(ENOUGH, Code::default());

        InflateState {
            mode: InflateMode::Head,
            last: false,
            wrap: 0,
            havedict: false,
            flags: -1,
            dmax: DEFAULT_DMAX,
            check: 0,
            total: 0,
            #[cfg(feature = "gzip")]
            head: None,
            wbits: 0,
            wsize: 0,
            whave: 0,
            wnext: 0,
            window: Vec::new(),
            hold: 0,
            bits: 0,
            length: 0,
            offset: 0,
            extra: 0,
            lencode: 0,
            distcode: 0,
            lenbits: 0,
            distbits: 0,
            ncode: 0,
            nlen: 0,
            ndist: 0,
            have: 0,
            next: 0,
            lens: [0; 320],
            work: [0; 288],
            codes: codes.into_boxed_slice(),
            sane: true,
            back: -1,
            was: 0,
        }
    }

    /// Re-initializes the engine-private state for stream reuse, mirroring C
    /// `inflateResetKeep` (`inflate.c`).
    ///
    /// This is the innermost of zlib's three reset entry points. It clears the
    /// decode position and header bookkeeping **without** touching the
    /// allocated [`window`](Self::window) or the
    /// [`wbits`](Self::wbits)/[`wrap`](Self::wrap)/[`wsize`](Self::wsize)
    /// configuration:
    ///
    /// * [`total`](Self::total) → `0`;
    /// * [`mode`](Self::mode) → [`InflateMode::Head`];
    /// * [`last`](Self::last)/[`havedict`](Self::havedict) → `false`;
    /// * [`flags`](Self::flags) → `-1`; [`dmax`](Self::dmax) → `32768`;
    /// * [`head`](Self::head) → `None` (gzip);
    /// * [`hold`](Self::hold)/[`bits`](Self::bits) → `0`;
    /// * [`lencode`](Self::lencode)/[`distcode`](Self::distcode)/[`next`](Self::next)
    ///   → `0` (the base of [`codes`](Self::codes));
    /// * [`sane`](Self::sane) → `true`; [`back`](Self::back) → `-1`.
    ///
    /// The wider resets (`inflateReset`, which additionally zeroes
    /// `wsize`/`whave`/`wnext`, and `inflateReset2`, which re-derives
    /// `wbits`/`wrap`) are composed on top of this in `crate::inflate`. The
    /// stream-level accounting (the [`ZStream`](crate::stream::ZStream) totals,
    /// `msg`, `adler`, `data_type`) is reset separately by
    /// [`ZStream::reset`](crate::stream::ZStream::reset), which invokes this
    /// method via the [`StreamState`] trait.
    pub fn reset_keep(&mut self) {
        self.total = 0;
        self.mode = InflateMode::Head;
        self.last = false;
        self.havedict = false;
        self.flags = -1;
        self.dmax = DEFAULT_DMAX;
        #[cfg(feature = "gzip")]
        {
            self.head = None;
        }
        self.hold = 0;
        self.bits = 0;
        self.lencode = 0;
        self.distcode = 0;
        self.next = 0;
        self.sane = true;
        self.back = -1;
    }
}

impl Default for InflateState {
    /// Equivalent to [`InflateState::new`].
    ///
    /// Provided so the type is constructible via [`Default`] and to satisfy
    /// `clippy::new_without_default`.
    fn default() -> Self {
        Self::new()
    }
}

// ===========================================================================
// StreamState — let ZStream own this state behind a trait object
// ===========================================================================

impl StreamState for InflateState {
    /// Always [`StreamKind::Inflate`]: this state drives decompression.
    fn kind(&self) -> StreamKind {
        StreamKind::Inflate
    }

    /// Resets the engine-private state for reuse by delegating to
    /// [`reset_keep`](InflateState::reset_keep) (the `inflateResetKeep`
    /// equivalent).
    fn reset(&mut self) {
        self.reset_keep();
    }

    /// Upcasts to [`&dyn Any`](Any) so an engine can downcast back to
    /// `InflateState` via [`ZStream::state_as`](crate::stream::ZStream::state_as).
    fn as_any(&self) -> &dyn Any {
        self
    }

    /// Mutable counterpart of [`as_any`](StreamState::as_any).
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

// ===========================================================================
// Drop — the idiomatic, leak-free replacement for `inflateEnd`
// ===========================================================================

impl Drop for InflateState {
    /// Deterministic, leak-free teardown — the idiomatic replacement for C
    /// `inflateEnd` (AAP §0.6.3).
    ///
    /// The body is intentionally empty: every owned field
    /// ([`window`](Self::window): [`Vec`], [`codes`](Self::codes): [`Box`]) frees
    /// its own backing storage when the struct is dropped, so there is **no
    /// manual free** and **no `unsafe`**. Because ownership is enforced by the
    /// borrow checker, double-free and use-after-free are unrepresentable; the
    /// `Drop` impl exists to document this contract (and to provide a single
    /// place for future debug-only invariant assertions should they be needed).
    fn drop(&mut self) {
        // Nothing to do: `Vec`/`Box` fields release their allocations
        // automatically. See the doc comment above.
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// The decode machine starts at `HEAD`, matching the value every reset
    /// returns to.
    #[test]
    fn default_mode_is_head() {
        assert_eq!(InflateMode::default(), InflateMode::Head);
    }

    /// The enum uses 32 zero-based discriminants (`Head == 0` … `Sync == 31`),
    /// independent of the C `HEAD = 16180` base.
    #[test]
    fn mode_discriminants_are_zero_based() {
        assert_eq!(InflateMode::Head as usize, 0);
        assert_eq!(InflateMode::Sync as usize, 31);
    }

    /// The two "first time in" modes are distinct from their steady-state
    /// counterparts (the driver relies on the distinction).
    #[test]
    fn begin_modes_are_distinct() {
        assert_ne!(InflateMode::CopyBegin, InflateMode::Copy);
        assert_ne!(InflateMode::LenBegin, InflateMode::Len);
    }

    /// A freshly constructed state has its buffers allocated and every field in
    /// its documented initial value.
    #[test]
    fn new_state_invariants() {
        let state = InflateState::new();
        assert_eq!(state.codes.len(), ENOUGH);
        assert_eq!(state.codes.len(), 1444);
        assert_eq!(state.mode, InflateMode::Head);
        assert!(state.window.is_empty());
        assert!(state.sane);
        assert_eq!(state.back, -1);
        assert_eq!(state.flags, -1);
        assert_eq!(state.dmax, 32768);
        assert_eq!(state.hold, 0);
        assert_eq!(state.bits, 0);
        assert_eq!(state.lencode, 0);
        assert_eq!(state.distcode, 0);
        assert_eq!(state.next, 0);
        assert_eq!(state.total, 0);
        assert!(!state.last);
        assert!(!state.havedict);
        // Every `codes` entry is the default (zeroed) `Code`.
        assert!(state.codes.iter().all(|&c| c == Code::default()));
    }

    /// `Default` is equivalent to `new()`.
    #[test]
    fn default_matches_new() {
        let a = InflateState::default();
        let b = InflateState::new();
        assert_eq!(a.mode, b.mode);
        assert_eq!(a.codes.len(), b.codes.len());
        assert_eq!(a.dmax, b.dmax);
        assert_eq!(a.back, b.back);
    }

    /// Cloning yields an independent state (sound because the state holds
    /// indices, not pointers — the `inflateCopy` contract).
    #[test]
    fn clone_is_independent() {
        let mut original = InflateState::new();
        original.lencode = 42;
        original.distcode = 7;
        original.next = 100;
        original.mode = InflateMode::Type;
        original.window = Vec::from([1u8, 2, 3, 4]);

        let mut cloned = original.clone();
        // The clone observes the same logical state...
        assert_eq!(cloned.lencode, 42);
        assert_eq!(cloned.distcode, 7);
        assert_eq!(cloned.next, 100);
        assert_eq!(cloned.mode, InflateMode::Type);
        assert_eq!(cloned.window, Vec::from([1u8, 2, 3, 4]));

        // ...but is a fully independent allocation.
        cloned.lencode = 0;
        cloned.window.clear();
        assert_eq!(original.lencode, 42);
        assert_eq!(original.window, Vec::from([1u8, 2, 3, 4]));
    }

    /// The state reports itself as inflate (decompression) state.
    #[test]
    fn kind_is_inflate() {
        let state = InflateState::new();
        assert_eq!(state.kind(), StreamKind::Inflate);
    }

    /// `reset_keep` (and the `StreamState::reset` that delegates to it) returns
    /// the decode position to its initial value without disturbing the window.
    #[test]
    fn reset_keep_restores_initial_decode_state() {
        let mut state = InflateState::new();

        // Dirty a representative set of fields, including an allocated window.
        state.mode = InflateMode::Len;
        state.last = true;
        state.havedict = true;
        state.flags = 0x88;
        state.hold = 0xDEAD_BEEF;
        state.bits = 17;
        state.lencode = 5;
        state.distcode = 9;
        state.next = 64;
        state.sane = false;
        state.back = 12;
        state.total = 9999;
        state.window = Vec::from([0xAAu8; 64]);
        state.wsize = 64;

        // `reset()` is the trait entry point; it must delegate to `reset_keep`.
        StreamState::reset(&mut state);

        assert_eq!(state.mode, InflateMode::Head);
        assert!(!state.last);
        assert!(!state.havedict);
        assert_eq!(state.flags, -1);
        assert_eq!(state.dmax, 32768);
        assert_eq!(state.hold, 0);
        assert_eq!(state.bits, 0);
        assert_eq!(state.lencode, 0);
        assert_eq!(state.distcode, 0);
        assert_eq!(state.next, 0);
        assert!(state.sane);
        assert_eq!(state.back, -1);
        assert_eq!(state.total, 0);

        // `reset_keep` deliberately preserves the window and its size.
        assert_eq!(state.window.len(), 64);
        assert_eq!(state.wsize, 64);
    }

    /// The `Any` upcast round-trips back to the concrete `InflateState`.
    #[test]
    fn as_any_downcasts_to_concrete() {
        let mut state = InflateState::new();
        state.bits = 3;

        let any_ref: &dyn Any = state.as_any();
        let recovered = any_ref
            .downcast_ref::<InflateState>()
            .expect("downcast back to InflateState");
        assert_eq!(recovered.bits, 3);

        let any_mut: &mut dyn Any = state.as_any_mut();
        let recovered_mut = any_mut
            .downcast_mut::<InflateState>()
            .expect("mutable downcast back to InflateState");
        recovered_mut.bits = 11;
        assert_eq!(state.bits, 11);
    }

    /// Header capture defaults to absent and is gzip-gated.
    #[cfg(feature = "gzip")]
    #[test]
    fn head_defaults_to_none() {
        let state = InflateState::new();
        assert!(state.head.is_none());
    }

    /// Sanity bound on the in-struct footprint: the large `codes` table is
    /// boxed out to the heap, so the inline size is dominated by the
    /// `lens`/`work` scratch arrays (1216 bytes) plus the scalar bookkeeping —
    /// comfortably under 8 KiB. This is a guard against an accidental inline of
    /// a large buffer, not an exact-layout assertion.
    #[test]
    fn struct_size_is_reasonable() {
        let size = core::mem::size_of::<InflateState>();
        // Must at least hold the two scratch arrays.
        assert!(size >= 320 * 2 + 288 * 2, "unexpectedly small: {size}");
        // Must not balloon (e.g. by inlining `codes` or the window).
        assert!(size < 8192, "unexpectedly large: {size}");
    }
}
