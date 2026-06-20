//! Internal inflate state: the [`InflateMode`] decode-state machine and the
//! [`InflateState`] struct.
//!
//! This module is the safe-Rust port of zlib's `inflate.h` — the *internal*
//! state definition shared by the main inflate engine (`crate::inflate`), the
//! fast inner loop (`crate::inflate::fast`), and the callback decompressor
//! (`crate::inflate::back`). It is deliberately a pure data module: it defines
//! the state *shape* and its construction/teardown, but contains **no** decode
//! logic (that lives in the engine modules that own and drive this state).
//!
//! # What the C `struct inflate_state` becomes
//!
//! The translation applies the AAP §0.6.2 / §0.6.3 ownership model uniformly:
//!
//! | C construct (`inflate.h`)            | Safe-Rust equivalent (here)                          |
//! |--------------------------------------|------------------------------------------------------|
//! | `z_streamp strm` (back-pointer)      | **omitted** — ownership is inverted (see below)      |
//! | `inflate_mode mode` (`int`-valued)   | [`InflateMode`] enum + exhaustive `match`            |
//! | `unsigned char FAR *window`          | owned [`Vec`]`<u8>` (lazily grown by `updatewindow`) |
//! | `code FAR *lencode` / `*distcode`    | [`usize`] **index** into [`codes`](InflateState::codes) |
//! | `code FAR *next`                     | [`usize`] **index** into [`codes`](InflateState::codes) |
//! | `code codes[ENOUGH]`                 | owned [`Box`]`<[`[`Code`]`]>` of length `ENOUGH`     |
//! | `unsigned long hold` / `check`       | [`u32`] (see the [`hold`](InflateState::hold) note)  |
//! | `unsigned long total`                | [`u64`]                                              |
//! | `gz_headerp head`                    | `Option<`[`GzHeader`](crate::gz_header::GzHeader)`>` (gzip feature) |
//! | `ZALLOC`/`ZFREE` + `inflateEnd`      | RAII: owned buffers freed by [`Drop`]                |
//!
//! ## Inverted ownership — no `strm` back-pointer
//!
//! In C, `inflate_state` holds a `z_streamp strm` back-pointer to the
//! `z_stream` that owns it, forming a cycle. In the Rust design ownership flows
//! strictly one way: a [`ZStream`](crate::stream::ZStream) owns its decode state
//! as `Option<Box<dyn `[`StreamState`](crate::stream::StreamState)`>>`, so the
//! back-pointer is **intentionally omitted**. The engine functions receive the
//! stream's input/output buffers as `&[u8]` / `&mut [u8]` *parameters* rather
//! than reaching them through a self-reference, which keeps `InflateState`
//! free of self-referential lifetimes and therefore trivially [`Clone`] and
//! movable.
//!
//! ## Pointers become indices
//!
//! The single most important transformation in this file: C stores the active
//! length/literal table, distance table, and next-free table slot as raw
//! `code *` pointers into the `codes[]` array. Here they become **`usize`
//! indices** ([`lencode`](InflateState::lencode),
//! [`distcode`](InflateState::distcode), [`next`](InflateState::next)) into the
//! owned [`codes`](InflateState::codes) slice. This is what makes
//! [`InflateState`] `Clone`-able for `inflateCopy`: cloning copies the `codes`
//! buffer and the *indices remain valid* against the copy, whereas raw pointers
//! would dangle into the original allocation and require re-basing.
//!
//! # Purity, `unsafe`, and `no_std`
//!
//! This module is **100% safe Rust** (zero `unsafe`, per AAP §0.7.2) and
//! **`no_std`-clean**: it relies only on `core` plus the `alloc` crate
//! ([`Box`]/[`Vec`]). It compiles under
//! `--no-default-features --features no-std`. The gzip header-capture field and
//! its type are gated behind `#[cfg(feature = "gzip")]`.
//!
//! # State-transition diagram (ported from `inflate.h`)
//!
//! Reproduced from the `inflate.h` comment block (Rust variant names in
//! parentheses where they differ from the C spelling). Most modes may also
//! transition to [`Bad`](InflateMode::Bad) or [`Mem`](InflateMode::Mem) on
//! error; those edges are omitted for clarity.
//!
//! ```text
//! Process header:
//!     HEAD -> (gzip) or (zlib) or (raw)
//!     (gzip) -> FLAGS -> TIME -> OS -> EXLEN -> EXTRA -> NAME -> COMMENT ->
//!               HCRC -> TYPE
//!     (zlib) -> DICTID or TYPE
//!     DICTID -> DICT -> TYPE
//!     (raw)  -> TYPEDO
//! Read deflate blocks:
//!     TYPE -> TYPEDO -> STORED or TABLE or LEN_ (LenBegin) or CHECK
//!     STORED -> COPY_ (CopyBegin) -> COPY -> TYPE
//!     TABLE -> LENLENS -> CODELENS -> LEN_ (LenBegin)
//!     LEN_ (LenBegin) -> LEN
//! Read deflate codes in fixed or dynamic block:
//!     LEN -> LENEXT or LIT or TYPE
//!     LENEXT -> DIST -> DISTEXT -> MATCH -> LEN
//!     LIT -> LEN
//! Process trailer:
//!     CHECK -> LENGTH -> DONE
//! ```

#[cfg(not(feature = "std"))]
use alloc::{boxed::Box, vec, vec::Vec};

use crate::inflate::tables::{Code, ENOUGH};
use crate::stream::{StreamKind, StreamState};

/// Possible inflate modes between `inflate()` calls.
///
/// This is the safe-Rust port of the C `inflate_mode` enum (`inflate.h`,
/// lines 20–53). It is the decode-state machine that the inflate engine
/// advances with an exhaustive `match`, replacing C's integer state plus
/// `switch`/`goto inf_leave` control flow (AAP §0.6.1).
///
/// # Discriminants are internal, not ABI
///
/// C assigns `HEAD = 16180` and lets the remaining variants auto-increment.
/// Those specific numbers are an implementation detail of the C build and are
/// **never** part of the wire format or the public ABI. Here the variants use
/// the natural **zero-based discriminants** Rust assigns by default (Rust enums
/// cannot be left uninitialised, so there is no reason to preserve the C base
/// value). The *order* is preserved exactly so that any ordinal reasoning in
/// the engine ports lines up with the C source.
///
/// # C name cross-reference
///
/// Two C modes have trailing-underscore names that are not valid idiomatic Rust
/// identifiers and denote "first time entering this state" variants. They are
/// renamed but kept **distinct** from their non-underscore counterparts because
/// the engine relies on the distinction (e.g. the `data_type` computation in
/// `crate::inflate` treats the "begin" variants specially, adding 256):
///
/// | C name  | Rust variant                  |
/// |---------|-------------------------------|
/// | `COPY_` | [`CopyBegin`](InflateMode::CopyBegin) |
/// | `LEN_`  | [`LenBegin`](InflateMode::LenBegin)   |
///
/// (`MATCH` maps to [`Match`](InflateMode::Match); `match` is a Rust keyword
/// but the capitalised identifier `Match` is not reserved, so no rename is
/// needed there.)
///
/// On a data error the engine moves to [`Bad`](InflateMode::Bad), which maps to
/// `Z_DATA_ERROR` at the API boundary (AAP §0.6.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum InflateMode {
    /// `HEAD` — waiting for the magic header. Initial mode after a reset.
    #[default]
    Head,
    /// `FLAGS` — waiting for method and flags (gzip).
    Flags,
    /// `TIME` — waiting for the modification time (gzip).
    Time,
    /// `OS` — waiting for extra flags and operating system (gzip).
    Os,
    /// `EXLEN` — waiting for the extra-field length (gzip).
    Exlen,
    /// `EXTRA` — waiting for the extra-field bytes (gzip).
    Extra,
    /// `NAME` — waiting for the end of the file name (gzip).
    Name,
    /// `COMMENT` — waiting for the end of the comment (gzip).
    Comment,
    /// `HCRC` — waiting for the header CRC (gzip).
    Hcrc,
    /// `DICTID` — waiting for the dictionary check value.
    Dictid,
    /// `DICT` — waiting for an `inflateSetDictionary()` call.
    Dict,
    /// `TYPE` — waiting for the block type bits, including the last-block flag.
    Type,
    /// `TYPEDO` — same as [`Type`](InflateMode::Type) but skips the check that
    /// would exit `inflate()` on a new block boundary.
    Typedo,
    /// `STORED` — waiting for the stored-block size (length and its complement).
    Stored,
    /// `COPY_` — first entry into the stored-block copy (one-shot setup variant
    /// of [`Copy`](InflateMode::Copy); see the type-level C name cross-reference).
    CopyBegin,
    /// `COPY` — waiting for input or output space to copy a stored block.
    Copy,
    /// `TABLE` — waiting for the dynamic-block table lengths.
    Table,
    /// `LENLENS` — waiting for the code-length code lengths.
    Lenlens,
    /// `CODELENS` — waiting for the length/literal and distance code lengths.
    Codelens,
    /// `LEN_` — first entry into code decoding (one-shot setup variant of
    /// [`Len`](InflateMode::Len); see the type-level C name cross-reference).
    LenBegin,
    /// `LEN` — waiting for a length/literal/end-of-block code.
    Len,
    /// `LENEXT` — waiting for the length extra bits.
    Lenext,
    /// `DIST` — waiting for a distance code.
    Dist,
    /// `DISTEXT` — waiting for the distance extra bits.
    Distext,
    /// `MATCH` — waiting for output space to copy a matched string.
    Match,
    /// `LIT` — waiting for output space to write a literal.
    Lit,
    /// `CHECK` — waiting for the 32-bit check value (trailer).
    Check,
    /// `LENGTH` — waiting for the 32-bit length (gzip trailer).
    Length,
    /// `DONE` — finished the check; remains here until reset.
    Done,
    /// `BAD` — a data error occurred; remains here until reset
    /// (maps to `Z_DATA_ERROR`).
    Bad,
    /// `MEM` — an `inflate()` memory error occurred; remains here until reset.
    Mem,
    /// `SYNC` — looking for synchronisation bytes to restart `inflate()`.
    Sync,
}

/// State maintained between `inflate()` calls.
///
/// This is the safe-Rust port of the C `struct inflate_state` (`inflate.h`,
/// lines 82–126). It is owned by a [`ZStream`](crate::stream::ZStream) as
/// `Option<Box<dyn `[`StreamState`](crate::stream::StreamState)`>>`; the C
/// `z_streamp strm` back-pointer is therefore **omitted** (see the
/// [module docs](self#inverted-ownership--no-strm-back-pointer)).
///
/// Approximate size, mirroring the C note, is on the order of 7 KiB plus the
/// allocated sliding [`window`](InflateState::window) (up to 32 KiB) — the bulk
/// is the fixed [`lens`](InflateState::lens)/[`work`](InflateState::work)
/// scratch arrays and the heap-owned [`codes`](InflateState::codes) table.
///
/// # `Clone` (for `inflateCopy`)
///
/// `InflateState` derives [`Clone`] so the public `inflateCopy` can duplicate a
/// decode stream. Cloning is sound precisely because
/// [`lencode`](InflateState::lencode), [`distcode`](InflateState::distcode), and
/// [`next`](InflateState::next) are **indices** into
/// [`codes`](InflateState::codes), not raw pointers: after a clone they address
/// the cloned buffer correctly with no pointer re-basing. The owned buffers
/// ([`Box`]`<[`[`Code`]`]>`, [`Vec`]`<u8>`), the fixed `[u16; N]` arrays, and the
/// optional [`GzHeader`](crate::gz_header::GzHeader) are all themselves
/// `Clone`.
#[derive(Clone)]
pub struct InflateState {
    // NOTE: C's `z_streamp strm` back-pointer is intentionally omitted — the
    // ZStream owns this state (`Option<Box<...>>`); engine functions receive
    // the stream's in/out slices as parameters instead (no self-reference).
    /// Current inflate mode (the decode-state machine position).
    pub mode: InflateMode,
    /// `true` while processing the last deflate block.
    pub last: bool,
    /// Wrapper/validation flags: bit 0 (`1`) = expect a zlib trailer (Adler-32),
    /// bit 1 (`2`) = gzip, bit 2 (`4`) = validate the check value. Mirrors the
    /// C `int wrap`.
    pub wrap: i32,
    /// `true` once a preset dictionary has been supplied.
    pub havedict: bool,
    /// gzip header method+flags byte; `0` for a zlib stream; `-1` for a raw
    /// stream or before any header has been seen.
    pub flags: i32,
    /// zlib-header maximum back-reference distance (the `INFLATE_STRICT` guard);
    /// defaults to `32768`.
    pub dmax: u32,
    /// Protected copy of the running check value (Adler-32 or CRC-32 — both are
    /// 32-bit, hence `u32`).
    pub check: u32,
    /// Protected copy of the cumulative output byte count (`u64`, matching the
    /// stream-level total).
    pub total: u64,
    /// Captured gzip header, when the caller requested it via
    /// `inflateGetHeader`. `Some` exactly when capture was requested; the gzip
    /// header-parse states fill its fields. Entirely gated behind the `gzip`
    /// feature, mirroring the C `#ifdef GUNZIP` build.
    #[cfg(feature = "gzip")]
    pub head: Option<crate::gz_header::GzHeader>,

    // ----- sliding window -------------------------------------------------
    /// Base-2 logarithm of the requested window size (`8..=15`).
    pub wbits: u32,
    /// Window size in bytes, or `0` when no window is in use yet.
    pub wsize: u32,
    /// Number of valid bytes currently in the [`window`](InflateState::window).
    pub whave: u32,
    /// Write cursor (index) into the [`window`](InflateState::window).
    pub wnext: u32,
    /// The sliding window. Allocated **lazily** by `updatewindow` (in
    /// `crate::inflate`) to `1 << wbits` bytes; it starts empty and stays empty
    /// while unused, replacing the C `unsigned char FAR *window` heap block.
    pub window: Vec<u8>,

    // ----- bit accumulator ------------------------------------------------
    /// Input bit accumulator.
    ///
    /// **Width (AAP §0.6.4):** `u32`, matching C's portable `unsigned long`
    /// path. `crate::inflate::fast` assumes a 2-byte refill whenever
    /// `bits < 15`; `crate::inflate` and `crate::inflate::back` must keep the
    /// identical refill cadence so the decoded output is byte-identical to C.
    pub hold: u32,
    /// Number of valid bits currently held in [`hold`](InflateState::hold).
    pub bits: u32,

    // ----- string / stored-block copy ------------------------------------
    /// Literal value, or the length of data remaining to copy.
    pub length: u32,
    /// Back-reference distance used to copy a matched string.
    pub offset: u32,

    // ----- table / code decoding -----------------------------------------
    /// Number of extra bits still needed for the current code.
    pub extra: u32,
    /// **Index** into [`codes`](InflateState::codes) of the root table for
    /// length/literal codes (C uses a `code *` pointer; here it is a `usize`
    /// index — the central pointer→index transformation, valid across clones).
    pub lencode: usize,
    /// **Index** into [`codes`](InflateState::codes) of the root table for
    /// distance codes (a `usize` index, like
    /// [`lencode`](InflateState::lencode)).
    pub distcode: usize,
    /// Number of index bits for the [`lencode`](InflateState::lencode) table.
    pub lenbits: u32,
    /// Number of index bits for the [`distcode`](InflateState::distcode) table.
    pub distbits: u32,

    // ----- dynamic-table building ----------------------------------------
    /// Number of code-length code lengths.
    pub ncode: u32,
    /// Number of length code lengths.
    pub nlen: u32,
    /// Number of distance code lengths.
    pub ndist: u32,
    /// Number of code lengths accumulated so far in
    /// [`lens`](InflateState::lens).
    pub have: u32,
    /// **Index** of the next available slot in [`codes`](InflateState::codes)
    /// while building tables (C uses a `code *` pointer; here a `usize` index).
    pub next: usize,
    /// Temporary storage for code lengths (C `unsigned short lens[320]`).
    pub lens: [u16; 320],
    /// Work area used while building the code tables (C
    /// `unsigned short work[288]`).
    pub work: [u16; 288],
    /// The owned decode-table buffer, length [`ENOUGH`] (`1444`). Replaces the
    /// C `code codes[ENOUGH]` inline array; indexed by
    /// [`lencode`](InflateState::lencode),
    /// [`distcode`](InflateState::distcode), and
    /// [`next`](InflateState::next).
    pub codes: Box<[Code]>,
    /// When `false`, allow an invalid (too-far) back-reference distance — the
    /// `INFLATE_ALLOW_INVALID_DISTANCE_TOOFAR_ARRR` escape hatch. Normally
    /// `true`.
    pub sane: bool,
    /// Bits back of the last unprocessed length/literal code; `-1` when there
    /// is no such pending code (hence `i32`).
    pub back: i32,
    /// Initial length of the match in progress.
    pub was: u32,
}

impl InflateState {
    /// Construct a fresh inflate state in its initial, post-reset condition.
    ///
    /// This allocates the owned [`codes`](InflateState::codes) buffer (length
    /// [`ENOUGH`], every entry a default [`Code`]) and leaves the
    /// [`window`](InflateState::window) **empty** — the window is grown lazily
    /// by `updatewindow` in `crate::inflate` to `1 << wbits` bytes the first
    /// time output must be retained. The field defaults mirror C zlib's
    /// `inflateReset` / `inflateResetKeep` (`inflate.c`): `mode = Head`,
    /// `flags = -1`, `dmax = 32768`, `sane = true`, `back = -1`, and all
    /// counters, the bit accumulator, and the table indices zeroed.
    ///
    /// The window-size and wrapper configuration (`wbits` / `wrap`) start at
    /// `0`; the heavier `inflateReset2` logic that decodes the `windowBits`
    /// argument and sets those lives in `crate::inflate`, which calls into this
    /// constructor and then configures the stream.
    #[must_use]
    pub fn new() -> InflateState {
        InflateState {
            mode: InflateMode::Head,
            last: false,
            wrap: 0,
            havedict: false,
            flags: -1,
            dmax: 32768,
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
            lens: [0u16; 320],
            work: [0u16; 288],
            codes: vec![Code::default(); ENOUGH].into_boxed_slice(),
            sane: true,
            back: -1,
            was: 0,
        }
    }
}

impl Default for InflateState {
    /// Equivalent to [`InflateState::new`].
    #[inline]
    fn default() -> InflateState {
        InflateState::new()
    }
}

impl StreamState for InflateState {
    /// Identifies this as a decompression state.
    #[inline]
    fn kind(&self) -> StreamKind {
        StreamKind::Inflate
    }

    /// Reset the decode state to its freshly-initialised condition **without**
    /// reallocating the owned buffers.
    ///
    /// This is a faithful port of the engine-internal portion of C
    /// `inflateReset` / `inflateResetKeep` (`inflate.c`): the running counter,
    /// bit accumulator, mode, gzip-header capture slot, table indices, and the
    /// window bookkeeping (`wsize`/`whave`/`wnext`) return to their initial
    /// values, while the already-allocated [`codes`](InflateState::codes) table
    /// and [`window`](InflateState::window) backing storage are **retained** (no
    /// reallocation). Exactly as in C, the configured window size and wrapper
    /// mode ([`wbits`](InflateState::wbits) / [`wrap`](InflateState::wrap)) are
    /// preserved — those are (re)configured by `inflateReset2` in
    /// `crate::inflate`, not here.
    fn reset(&mut self) {
        // inflateReset(): clear the window bookkeeping (the backing `window`
        // Vec is retained, just logically emptied).
        self.wsize = 0;
        self.whave = 0;
        self.wnext = 0;

        // inflateResetKeep(): the rest of the per-stream decode state.
        self.total = 0;
        self.mode = InflateMode::Head;
        self.last = false;
        self.havedict = false;
        self.flags = -1;
        self.dmax = 32768;
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

    /// Opt in to concrete-type recovery from a `dyn StreamState`.
    ///
    /// The gzip read/write layer (`crate::gz`) stores this inflate engine inside
    /// a [`ZStream`](crate::stream::ZStream) as a `Box<dyn StreamState>` but
    /// drives it through the inflate *free functions* (`inflate`,
    /// `inflate_reset`), which require a `&mut InflateState`. Returning
    /// `Some(self)` here lets those callers recover the concrete reference via a
    /// checked [`core::any::Any::downcast_mut`], keeping the engine types out of
    /// the `stream` module entirely.
    #[inline]
    fn as_any_mut(&mut self) -> Option<&mut dyn core::any::Any> {
        Some(self)
    }
}

impl Drop for InflateState {
    /// Deterministic, leak-free teardown — the RAII replacement for the C
    /// `inflateEnd` function (AAP §0.6.3).
    ///
    /// Every owned resource frees itself automatically: the
    /// [`codes`](InflateState::codes) [`Box`] and the
    /// [`window`](InflateState::window) [`Vec`] (and, under the `gzip` feature,
    /// the captured [`head`](InflateState::head)) run their own destructors when
    /// this value is dropped. There is therefore **no manual free and no
    /// `unsafe`** here; double-free and use-after-free are unrepresentable
    /// because ownership is enforced by the borrow checker. The explicit `impl`
    /// documents that dropping an [`InflateState`] *is* the end-of-stream
    /// cleanup callers would invoke `inflateEnd` for in C.
    fn drop(&mut self) {
        // Intentionally empty: all fields are owned and self-freeing.
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_default_is_head() {
        assert_eq!(InflateMode::default(), InflateMode::Head);
    }

    #[test]
    fn mode_discriminants_are_zero_based_and_ordered() {
        // The engine ports rely on the variant *order* (and the renamed
        // "begin" variants staying distinct). Lock the zero-based layout.
        assert_eq!(InflateMode::Head as usize, 0);
        assert_eq!(InflateMode::Flags as usize, 1);
        assert_eq!(InflateMode::CopyBegin as usize, 14);
        assert_eq!(InflateMode::Copy as usize, 15);
        assert_eq!(InflateMode::LenBegin as usize, 19);
        assert_eq!(InflateMode::Len as usize, 20);
        assert_eq!(InflateMode::Match as usize, 24);
        assert_eq!(InflateMode::Sync as usize, 31);
        // "begin" variants must differ from their steady-state counterparts.
        assert_ne!(InflateMode::CopyBegin, InflateMode::Copy);
        assert_ne!(InflateMode::LenBegin, InflateMode::Len);
    }

    #[test]
    fn new_state_has_expected_initial_values() {
        let s = InflateState::new();
        assert_eq!(s.codes.len(), ENOUGH);
        assert_eq!(s.codes.len(), 1444);
        assert_eq!(s.mode, InflateMode::Head);
        assert!(s.window.is_empty());
        assert!(s.sane);
        assert_eq!(s.back, -1);
        assert_eq!(s.flags, -1);
        assert_eq!(s.dmax, 32768);
        assert_eq!(s.wrap, 0);
        assert_eq!(s.wbits, 0);
        assert_eq!(s.lencode, 0);
        assert_eq!(s.distcode, 0);
        assert_eq!(s.next, 0);
        assert_eq!(s.total, 0);
        assert_eq!(s.hold, 0);
        assert_eq!(s.bits, 0);
        assert!(!s.last);
        assert!(!s.havedict);
        // Every code entry starts as the default (zeroed) `Code`.
        assert!(s.codes.iter().all(|c| *c == Code::default()));
    }

    #[test]
    fn default_matches_new() {
        let a = InflateState::default();
        let b = InflateState::new();
        assert_eq!(a.mode, b.mode);
        assert_eq!(a.codes.len(), b.codes.len());
        assert_eq!(a.sane, b.sane);
        assert_eq!(a.back, b.back);
    }

    #[test]
    fn clone_is_independent_and_indices_stay_valid() {
        let mut original = InflateState::new();
        original.mode = InflateMode::Len;
        original.lencode = 7;
        original.distcode = 11;
        original.next = 23;
        original.codes[7] = Code::new(0, 8, 0x55);

        let cloned = original.clone();

        // The clone observes the same logical state...
        assert_eq!(cloned.mode, InflateMode::Len);
        assert_eq!(cloned.lencode, 7);
        assert_eq!(cloned.distcode, 11);
        assert_eq!(cloned.next, 23);
        // ...and the indices address the cloned buffer, not the original.
        assert_eq!(cloned.codes.len(), ENOUGH);
        assert_eq!(cloned.codes[cloned.lencode], Code::new(0, 8, 0x55));

        // Mutating the clone must not affect the original (independent buffers).
        let mut cloned = cloned;
        cloned.codes[7] = Code::new(0, 9, 0x11);
        cloned.mode = InflateMode::Bad;
        assert_eq!(original.codes[7], Code::new(0, 8, 0x55));
        assert_eq!(original.mode, InflateMode::Len);
    }

    #[test]
    fn stream_state_kind_is_inflate() {
        let s = InflateState::new();
        assert_eq!(s.kind(), StreamKind::Inflate);
        assert!(s.is_inflate());
        assert!(!s.is_deflate());
    }

    #[test]
    fn reset_restores_defaults_but_keeps_buffers_and_config() {
        let mut s = InflateState::new();
        // Configure window/wrapper and grow the window buffer, then dirty the
        // transient decode state.
        s.wbits = 15;
        s.wrap = 2; // gzip
        s.window = vec![0xAB; 1 << 15];
        let window_cap = s.window.capacity();
        s.wsize = 1 << 15;
        s.whave = 100;
        s.wnext = 50;
        s.mode = InflateMode::Len;
        s.last = true;
        s.havedict = true;
        s.flags = 8;
        s.dmax = 123;
        s.hold = 0xDEAD;
        s.bits = 13;
        s.lencode = 42;
        s.distcode = 99;
        s.next = 100;
        s.sane = false;
        s.back = 7;
        s.total = 9999;

        let codes_ptr = s.codes.as_ptr();
        s.reset();

        // inflateResetKeep fields cleared:
        assert_eq!(s.mode, InflateMode::Head);
        assert!(!s.last);
        assert!(!s.havedict);
        assert_eq!(s.flags, -1);
        assert_eq!(s.dmax, 32768);
        assert_eq!(s.hold, 0);
        assert_eq!(s.bits, 0);
        assert_eq!(s.lencode, 0);
        assert_eq!(s.distcode, 0);
        assert_eq!(s.next, 0);
        assert!(s.sane);
        assert_eq!(s.back, -1);
        assert_eq!(s.total, 0);
        // inflateReset window bookkeeping cleared:
        assert_eq!(s.wsize, 0);
        assert_eq!(s.whave, 0);
        assert_eq!(s.wnext, 0);
        // Configuration preserved (set by reset2, not cleared by reset):
        assert_eq!(s.wbits, 15);
        assert_eq!(s.wrap, 2);
        // Buffers retained, NOT reallocated:
        assert_eq!(s.codes.len(), ENOUGH);
        assert_eq!(s.codes.as_ptr(), codes_ptr);
        assert_eq!(s.window.capacity(), window_cap);
    }

    #[cfg(feature = "gzip")]
    #[test]
    fn gzip_head_starts_none_and_reset_clears_it() {
        let mut s = InflateState::new();
        assert!(s.head.is_none());
        s.head = Some(crate::gz_header::GzHeader::new());
        s.reset();
        assert!(s.head.is_none());
    }

    #[test]
    fn size_of_is_reasonable() {
        // Sanity bound only (not an exact ABI assertion): the struct holds the
        // two fixed scratch arrays (320 + 288 `u16` = 1216 bytes) plus assorted
        // scalars and three pointer-sized owned handles. It must comfortably
        // exceed the arrays and stay well under the C "approximately 7K" note.
        let sz = core::mem::size_of::<InflateState>();
        assert!(sz >= 1216, "unexpectedly small: {sz}");
        assert!(sz <= 8192, "unexpectedly large: {sz}");
    }
}
