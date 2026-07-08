//! Inflate state model: [`InflateMode`] (C `inflate_mode`) and [`InflateState`]
//! (C `struct inflate_state`) — a safe Rust port of `inflate.h`.
//!
//! This module defines the **data model** of the DEFLATE decompression state
//! machine. It contains no decoding logic; it merely holds the fields that must
//! persist between `inflate()` calls. The driver ([`crate::inflate`] `mod.rs`),
//! the fast decode loop (`fast.rs`), and the raw-callback back-inflate
//! (`back.rs`) all operate on an [`InflateState`], and the public streaming
//! type [`crate::stream`] owns one as `Option<Box<InflateState>>`.
//!
//! # Relationship to the C model
//!
//! The layout mirrors `struct inflate_state` from `inflate.h` field-for-field,
//! with three idiomatic transformations that make the whole decoder (outside
//! `fast.rs`) expressible in **safe** Rust:
//!
//! * **No back-pointer.** The C struct stores `z_streamp strm` — a pointer back
//!   to the owning stream. Ownership is inverted here: the stream owns the
//!   state (`Option<Box<InflateState>>`, per AAP §0.6.3), so the back-pointer is
//!   omitted entirely rather than modelled as a raw pointer.
//! * **Owned window.** The C `unsigned char FAR *window` (manually
//!   `ZALLOC`/`ZFREE`-managed) becomes an owned [`Vec<u8>`]. Dropping the
//!   [`InflateState`] frees the window automatically, so RAII subsumes the
//!   `ZFREE(state->window)` performed by C `inflateEnd` (AAP §0.3.2).
//! * **Table offsets, not self-referential pointers.** The C `lencode` /
//!   `distcode` are `code const FAR *` pointers that may point *into* the
//!   state's own `codes[]` arena (dynamic Huffman blocks) or into the
//!   module-static fixed tables (`inffixed.h`). A self-referential borrow into
//!   an owned array cannot be expressed in safe Rust, so those pointers become
//!   `usize` offsets into [`codes`](InflateState::codes) paired with a
//!   [`TableSource`] discriminator. See [`TableSource`] and
//!   [`InflateState::lencode_slice`] for the resolution mechanism.
//!
//! # Safety
//!
//! This module contains **zero `unsafe`** (AAP §0.6.2 — only `fast.rs` may use
//! `unsafe` in the inflate layer) and is `no_std`-compatible: it references only
//! `core`, `alloc`, and the crate's own safe modules.

use alloc::boxed::Box;

use crate::gz_header::GzHeader;
use crate::inflate::tables::{Code, ENOUGH};
use crate::stream::{AllocBuffer, AllocHook};

/// The possible inflate modes maintained between `inflate()` calls.
///
/// This is the safe Rust equivalent of the C `inflate_mode` enum (`inflate.h`).
/// The discriminants begin at the distinctive sentinel value `16180` (`HEAD`)
/// and increment sequentially; the C code uses that non-zero base as a cheap
/// "is this a validly initialized state?" check in `inflateStateCheck`. The
/// exact numeric values are preserved so that any debugging tooling or memory
/// dump matches reference zlib, and so the state occupies the same conceptual
/// range (`16180..=16211`).
///
/// The C `switch`/`goto` fall-through dispatch over these modes is reshaped in
/// the driver into a loop-plus-`match`; Rust's exhaustiveness checking then
/// guarantees no mode is silently unhandled (AAP §0.6.1).
///
/// # State transitions (from `inflate.h`)
///
/// ```text
/// Header:  HEAD -> (gzip) FLAGS..HCRC -> TYPE
///               -> (zlib) DICTID -> DICT -> TYPE  |  TYPE
///               -> (raw)  TYPEDO
/// Blocks:  TYPE -> TYPEDO -> STORED | TABLE | LEN_ | CHECK
///          STORED -> COPY_ -> COPY -> TYPE
///          TABLE  -> LENLENS -> CODELENS -> LEN_ -> LEN
/// Codes:   LEN -> LENEXT | LIT | TYPE ;  LENEXT -> DIST -> DISTEXT -> MATCH -> LEN
/// Trailer: CHECK -> LENGTH -> DONE
/// ```
///
/// (Most modes may additionally transition to [`Bad`](InflateMode::Bad) or
/// [`Mem`](InflateMode::Mem) on error.)
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u16)] // values start at 16180; u16 suffices (max SYNC == 16180 + 31 == 16211)
pub enum InflateMode {
    /// i: waiting for the magic header (zlib CMF+FLG or gzip magic). The
    /// sentinel start value (`16180`) of the C `inflate_mode` enum.
    Head = 16180,
    /// i: waiting for method and flags (gzip).
    Flags,
    /// i: waiting for the modification time (gzip).
    Time,
    /// i: waiting for the extra flags and operating system (gzip).
    Os,
    /// i: waiting for the extra-field length (gzip).
    ExLen,
    /// i: waiting for the extra-field bytes (gzip).
    Extra,
    /// i: waiting for the end of the file name (gzip).
    Name,
    /// i: waiting for the end of the comment (gzip).
    Comment,
    /// i: waiting for the header CRC (gzip).
    Hcrc,
    /// i: waiting for the dictionary check value.
    DictId,
    /// Waiting for an `inflateSetDictionary()` call.
    Dict,
    /// i: waiting for type bits, including the last-block flag bit.
    Type,
    /// i: same as [`Type`](InflateMode::Type), but skips the check so
    /// `inflate()` can exit on a new block boundary.
    TypeDo,
    /// i: waiting for the stored-block size (length and its one's complement).
    Stored,
    /// i/o: same as [`Copy`](InflateMode::Copy) below, but only the first time
    /// in. Corresponds to the C mode `COPY_` (the trailing underscore denotes
    /// the "first time in" variant).
    CopyUnderscore,
    /// i/o: waiting for input or output space to copy a stored block.
    Copy,
    /// i: waiting for the dynamic-block table lengths.
    Table,
    /// i: waiting for the code-length code lengths.
    LenLens,
    /// i: waiting for the length/literal and distance code lengths.
    CodeLens,
    /// i: same as [`Len`](InflateMode::Len) below, but only the first time in.
    /// Corresponds to the C mode `LEN_` (the trailing underscore denotes the
    /// "first time in" variant).
    LenUnderscore,
    /// i: waiting for a length/literal/end-of-block code.
    Len,
    /// i: waiting for the length extra bits.
    LenExt,
    /// i: waiting for a distance code.
    Dist,
    /// i: waiting for the distance extra bits.
    DistExt,
    /// o: waiting for output space to copy a matched string.
    Match,
    /// o: waiting for output space to write a literal.
    Lit,
    /// i: waiting for the 32-bit check value.
    Check,
    /// i: waiting for the 32-bit length (gzip trailer).
    Length,
    /// Finished the trailing check — done; remains here until reset.
    Done,
    /// Got a data error — remains here until reset.
    Bad,
    /// Got an `inflate()` memory error — remains here until reset.
    Mem,
    /// Looking for synchronization bytes to restart `inflate()`.
    Sync,
}

impl Default for InflateMode {
    /// The mode a freshly initialized or reset inflate state starts in.
    ///
    /// Reference zlib sets `state->mode = HEAD` in `inflateResetKeep`; this
    /// mirrors that reset value.
    #[inline]
    fn default() -> Self {
        InflateMode::Head
    }
}

/// Identifies which table arena the active decode table lives in.
///
/// The C `struct inflate_state` stores `lencode` / `distcode` as
/// `code const FAR *` pointers. Those pointers may reference **either**:
///
/// * the state's own [`codes`](InflateState::codes) arena — for the Huffman
///   tables built from a *dynamic* DEFLATE block; or
/// * the module-static *fixed* tables (`inffixed.h` → `crate::inflate::fixed`'s
///   `LENFIX` / `DISTFIX`) — used for a *fixed* DEFLATE block.
///
/// Reference zlib distinguishes the two cases purely by pointer value
/// (`inflateCopy` checks whether the pointer falls inside `codes[]`). A
/// self-referential borrow cannot be held safely in Rust, so this discriminator
/// records the choice explicitly. It is paired with the `usize` offsets
/// [`InflateState::lencode`] / [`InflateState::distcode`], which are meaningful
/// only in the [`Dynamic`](TableSource::Dynamic) case, and it is resolved to a
/// concrete slice by [`InflateState::lencode_slice`] /
/// [`InflateState::distcode_slice`].
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
pub enum TableSource {
    /// The active table is one of the module-static fixed tables
    /// (`crate::inflate::fixed::LENFIX` / `DISTFIX`).
    Fixed,
    /// The active table lives in [`InflateState::codes`], starting at the
    /// matching offset ([`InflateState::lencode`] or
    /// [`InflateState::distcode`]).
    ///
    /// This is the default because a freshly reset state points
    /// `lencode`/`distcode` at the start of its own `codes[]` arena, exactly as
    /// C's `inflateResetKeep` does (`lencode = distcode = next = codes`).
    #[default]
    Dynamic,
}

/// State maintained between `inflate()` calls.
///
/// This is the safe Rust port of the C `struct inflate_state` (`inflate.h`).
/// It occupies roughly 7 KB (dominated by the [`codes`](InflateState::codes)
/// decode-table arena and the [`lens`](InflateState::lens) /
/// [`work`](InflateState::work) scratch arrays), not counting the sliding
/// [`window`](InflateState::window), which is allocated on demand and can grow
/// to 32 KB.
///
/// Because the struct is large, it is normally kept behind a [`Box`] (see
/// [`InflateState::new`]); the owning [`crate::stream`] type holds it as
/// `Option<Box<InflateState>>`.
///
/// # Ownership and cleanup
///
/// All buffers are owned (the [`window`](InflateState::window) is a [`Vec<u8>`];
/// the tables are inline arrays). No manual teardown is required: dropping the
/// `Box<InflateState>` frees the window automatically, which is precisely how
/// Rust ownership subsumes the `ZFREE(state->window)` that C `inflateEnd`
/// performs (AAP §0.3.2). No explicit [`Drop`] implementation is needed.
///
/// # Field width notes
///
/// * [`hold`](InflateState::hold) / [`bits`](InflateState::bits) are [`u32`]
///   even though C declares `hold` as `unsigned long`. The inflate algorithm
///   never needs more than 32 accumulated bits (`NEEDBITS(32)` is the maximum,
///   reached only for the stored-block length/complement), and the final
///   `PULLBYTE` before that limit shifts by at most 24, so a [`u32`]
///   accumulator holds every value the algorithm produces without truncation.
///   All `NEEDBITS`/`DROPBITS` arithmetic in `mod.rs` / `back.rs` must therefore
///   also use [`u32`].
/// * [`total`](InflateState::total) is [`u64`] to match the 64-bit `z_size_t` /
///   `unsigned long total` used by reference zlib and by `inflateMark`.
/// * [`back`](InflateState::back) is signed ([`i32`]) because it holds the
///   sentinel `-1` ("no unprocessed length/literal code yet").
pub struct InflateState {
    // --- state / framing ---------------------------------------------------
    /// Current inflate mode (the C `mode` field driving the state machine).
    pub mode: InflateMode,
    /// `true` while processing the last block of the stream (C `int last`).
    pub last: bool,
    /// Wrapper selector: bit 0 = expect a zlib header/trailer, bit 1 = expect a
    /// gzip header/trailer, bit 2 = validate the check value (C `int wrap`).
    pub wrap: i32,
    /// `true` once a preset dictionary has been supplied (C `int havedict`).
    pub havedict: bool,
    /// gzip header method and flags: `0` for a zlib stream, or `-1` for a raw
    /// stream / before any header has been seen (C `int flags`).
    pub flags: i32,
    /// zlib-header maximum back-reference distance, used by the `INFLATE_STRICT`
    /// distance check (C `unsigned dmax`). Defaults to `32768` (`1 << 15`).
    pub dmax: u32,
    /// Protected copy of the running check value (Adler-32 for zlib, CRC-32 for
    /// gzip). Kept separate from the stream's `adler` field (C
    /// `unsigned long check`).
    pub check: u32,
    /// Protected copy of the total number of output bytes produced (C
    /// `unsigned long total`; widened to [`u64`] here).
    pub total: u64,
    /// Optional gzip header being filled in when the caller requested one via
    /// `inflateGetHeader` (C `gz_headerp head`; [`None`] mirrors `Z_NULL`).
    pub head: Option<GzHeader>,

    // --- sliding window ----------------------------------------------------
    /// Base-2 logarithm of the requested window size (C `unsigned wbits`).
    pub wbits: u32,
    /// Window size in bytes, or `0` when the window is not yet in use (C
    /// `unsigned wsize`). The `0` sentinel mirrors C's "window not initialized"
    /// state.
    pub wsize: u32,
    /// Number of valid bytes currently stored in the window (C
    /// `unsigned whave`).
    pub whave: u32,
    /// Write index (next position to write) within the circular window (C
    /// `unsigned wnext`).
    pub wnext: u32,
    /// Owned sliding window, allocated on demand.
    ///
    /// Starts empty ([`AllocBuffer::default`]) and is sized to `1 << wbits` on
    /// first use by the driver's `updatewindow`, mirroring C's lazy `ZALLOC` of
    /// `state->window`. When a caller installed `zalloc`/`zfree` through the FFI
    /// `z_stream`, the window is routed through those hooks via the
    /// [`alloc_hook`](InflateState::alloc_hook) stored at construction time
    /// (AAP §0.6.3; QA FINDING-3); otherwise it is a plain global allocation.
    /// Being owned, it is freed automatically on drop — through the caller's
    /// `zfree` for a hook-backed buffer, or the global allocator otherwise —
    /// subsuming C's `ZFREE(state->window)`.
    pub window: AllocBuffer<u8>,

    // --- bit accumulator ---------------------------------------------------
    /// Input bit accumulator; only the low 32 bits are ever used (C
    /// `unsigned long hold`, narrowed to [`u32`] — see the type note on
    /// [`InflateState`]).
    pub hold: u32,
    /// Number of valid bits currently held in [`hold`](InflateState::hold) (C
    /// `unsigned bits`).
    pub bits: u32,

    // --- string / stored-block copy ---------------------------------------
    /// Literal byte value, or the length of data still to copy (C
    /// `unsigned length`).
    pub length: u32,
    /// Distance back into the window from which to copy a matched string (C
    /// `unsigned offset`).
    pub offset: u32,

    // --- table / code decoding --------------------------------------------
    /// Number of extra bits still needed for the current length or distance
    /// code (C `unsigned extra`).
    pub extra: u32,
    /// Offset into [`codes`](InflateState::codes) of the starting
    /// literal/length decode table, valid when
    /// [`lentable`](InflateState::lentable) is [`TableSource::Dynamic`] (C
    /// `code const FAR *lencode`, represented as a `usize` offset).
    pub lencode: usize,
    /// Offset into [`codes`](InflateState::codes) of the starting distance
    /// decode table, valid when [`disttable`](InflateState::disttable) is
    /// [`TableSource::Dynamic`] (C `code const FAR *distcode`, represented as a
    /// `usize` offset).
    pub distcode: usize,
    /// Which arena the active literal/length table lives in (fixed vs dynamic).
    /// See [`TableSource`] and [`InflateState::lencode_slice`].
    pub lentable: TableSource,
    /// Which arena the active distance table lives in (fixed vs dynamic). See
    /// [`TableSource`] and [`InflateState::distcode_slice`].
    pub disttable: TableSource,
    /// Root-table index bits for the literal/length table (C
    /// `unsigned lenbits`).
    pub lenbits: u32,
    /// Root-table index bits for the distance table (C `unsigned distbits`).
    pub distbits: u32,

    // --- dynamic table building -------------------------------------------
    /// Number of code-length code lengths (C `unsigned ncode`).
    pub ncode: u32,
    /// Number of literal/length code lengths (C `unsigned nlen`).
    pub nlen: u32,
    /// Number of distance code lengths (C `unsigned ndist`).
    pub ndist: u32,
    /// Number of code lengths already read into [`lens`](InflateState::lens) (C
    /// `unsigned have`).
    pub have: u32,
    /// Next available slot in [`codes`](InflateState::codes) while building
    /// tables (C `code FAR *next`, represented as a `usize` offset). This is the
    /// cursor advanced by `crate::inflate::tables::inflate_table`.
    pub next: usize,
    /// Temporary storage for code lengths (C `unsigned short lens[320]`).
    pub lens: [u16; 320],
    /// Scratch work area used while building the decode tables (C
    /// `unsigned short work[288]`).
    pub work: [u16; 288],

    // --- decode-table arena ------------------------------------------------
    /// Space for the dynamically built literal/length and distance decode
    /// tables (C `code codes[ENOUGH]`).
    ///
    /// Sized to [`ENOUGH`] (`1444` = `ENOUGH_LENS` `852` + `ENOUGH_DISTS`
    /// `592`), exactly matching the reference C footprint and the table-overflow
    /// guard in `inflate_table`.
    pub codes: [Code; ENOUGH],

    // --- error recovery / validation --------------------------------------
    /// When `false`, permit an otherwise-invalid "distance too far back"
    /// reference (used by `inflateSync` recovery). `true` in normal operation
    /// (C `int sane`).
    pub sane: bool,
    /// Number of bits back of the last unprocessed length/literal code; `-1`
    /// when there is none. Must stay signed (C `int back`).
    pub back: i32,
    /// Initial length of the match currently being processed (C
    /// `unsigned was`).
    pub was: u32,

    /// Allocator hook threaded from the owning stream at construction time.
    ///
    /// The C `struct inflate_state` has no such field because the manual
    /// `ZALLOC`/`ZFREE` calls read `strm->zalloc`/`strm->zfree` directly. Here
    /// the state owns its [`window`](InflateState::window) rather than holding a
    /// back-pointer to the stream, so the hook is captured once (by
    /// [`new_in`](InflateState::new_in)) and consulted whenever the window is
    /// lazily (re)allocated. It is [`AllocHook::none`] under the Rust global
    /// allocator, or the caller's `zalloc`/`zfree`/`opaque` when one was
    /// installed through the FFI `z_stream` (AAP §0.6.3; QA FINDING-3).
    pub alloc_hook: AllocHook,
}

/// Default `dmax` value: the maximum back-reference distance for a 32 KiB
/// window (`1 << 15`). Reference zlib initializes `state->dmax = 32768U` in
/// `inflateResetKeep`.
const DMAX_DEFAULT: u32 = 32768;

impl InflateState {
    /// Creates a new, boxed inflate state for the given wrapper mode and window
    /// size.
    ///
    /// `wrap` is the already-decoded wrapper selector (bit 0 = zlib, bit 1 =
    /// gzip, bit 2 = validate check value) and `wbits` is the base-2 logarithm
    /// of the window size; both are computed by the driver from the caller's
    /// overloaded `windowBits` argument (see
    /// [`crate::constants::parse_window_bits`]). The returned state reproduces
    /// the field values reference zlib leaves after
    /// `inflateInit2_` + `inflateReset2` + `inflateResetKeep`:
    ///
    /// * [`mode`](InflateState::mode) = [`InflateMode::Head`],
    /// * [`flags`](InflateState::flags) = `-1` (no header seen yet),
    /// * [`dmax`](InflateState::dmax) = `32768`,
    /// * [`sane`](InflateState::sane) = `true`, [`back`](InflateState::back) =
    ///   `-1`,
    /// * the decode-table cursors ([`lencode`](InflateState::lencode),
    ///   [`distcode`](InflateState::distcode), [`next`](InflateState::next)) at
    ///   offset `0` with [`TableSource::Dynamic`] tables,
    /// * and an **empty** [`window`](InflateState::window) — the allocation is
    ///   deferred until the driver's `updatewindow` first needs it, matching C's
    ///   lazy `ZALLOC`.
    ///
    /// The state is returned already boxed because it is large (~7 KB): keeping
    /// it behind a [`Box`] matches the `Option<Box<InflateState>>` the owning
    /// [`crate::stream`] type holds, and avoids moving the arrays around by
    /// value.
    ///
    /// # Allocator
    ///
    /// This convenience constructor records [`AllocHook::none`], so the window
    /// (when the driver later allocates it) uses the Rust global allocator —
    /// equivalent to a C caller with null `zalloc`/`zfree`. The FFI init path
    /// calls [`new_in`](Self::new_in) instead, passing the caller's
    /// [`AllocHook`] so the window is routed through the caller's
    /// `zalloc`/`zfree` (AAP §0.6.3).
    #[inline]
    #[must_use]
    pub fn new(wrap: i32, wbits: u32) -> Box<InflateState> {
        Self::new_in(AllocHook::none(), wrap, wbits)
    }

    /// Builds a boxed [`InflateState`], recording the supplied [`AllocHook`] so
    /// the lazily-allocated [`window`](InflateState::window) is routed through
    /// the caller's `zalloc`/`zfree` when active, or the global allocator
    /// otherwise. See [`new`](Self::new) for the full field-initialization
    /// contract; this is the same constructor with an explicit allocator hook
    /// (AAP §0.6.3 has-hook clause; QA FINDING-3).
    ///
    /// The window is *not* allocated here — it is sized on demand by the
    /// driver's `updatewindow`, which consults the stored
    /// [`alloc_hook`](InflateState::alloc_hook) at that time.
    #[must_use]
    pub fn new_in(hook: AllocHook, wrap: i32, wbits: u32) -> Box<InflateState> {
        Box::new(InflateState {
            // reset-managed fields (see `reset_keep`)
            mode: InflateMode::Head,
            last: false,
            havedict: false,
            flags: -1,
            dmax: DMAX_DEFAULT,
            total: 0,
            head: None,
            hold: 0,
            bits: 0,
            lencode: 0,
            distcode: 0,
            lentable: TableSource::Dynamic,
            disttable: TableSource::Dynamic,
            next: 0,
            sane: true,
            back: -1,
            // caller-provided framing / window geometry
            wrap,
            wbits,
            // kept / lazily-sized buffers and per-block scratch
            check: 0,
            wsize: 0,
            whave: 0,
            wnext: 0,
            window: AllocBuffer::default(),
            length: 0,
            offset: 0,
            extra: 0,
            lenbits: 0,
            distbits: 0,
            ncode: 0,
            nlen: 0,
            ndist: 0,
            have: 0,
            lens: [0; 320],
            work: [0; 288],
            codes: [Code::default(); ENOUGH],
            was: 0,
            alloc_hook: hook,
        })
    }

    /// Resets the per-stream decoding fields while **keeping** the sliding
    /// window allocation and its contents.
    ///
    /// This is the state-level portion of the C `inflateResetKeep` routine: it
    /// restores [`mode`](InflateState::mode) to [`InflateMode::Head`] and clears
    /// the block/bit-buffer/table state so decoding can restart, but it leaves
    /// [`wrap`](InflateState::wrap), [`wbits`](InflateState::wbits), and the
    /// [`window`](InflateState::window) (plus [`wsize`](InflateState::wsize) /
    /// [`whave`](InflateState::whave) / [`wnext`](InflateState::wnext))
    /// untouched. The stream-level counterparts (`total_in`/`total_out`, `msg`,
    /// `adler`) are reset by the driver in `mod.rs`, which calls this helper.
    pub fn reset_keep(&mut self) {
        self.total = 0;
        self.mode = InflateMode::Head;
        self.last = false;
        self.havedict = false;
        self.flags = -1;
        self.dmax = DMAX_DEFAULT;
        self.head = None;
        self.hold = 0;
        self.bits = 0;
        // `lencode = distcode = next = codes` (offset 0, dynamic arena) in C.
        self.lencode = 0;
        self.distcode = 0;
        self.next = 0;
        self.lentable = TableSource::Dynamic;
        self.disttable = TableSource::Dynamic;
        self.sane = true;
        self.back = -1;
    }

    /// Resets the state and additionally invalidates the sliding window
    /// contents.
    ///
    /// This mirrors the state-level portion of the C `inflateReset` routine,
    /// which zeroes [`wsize`](InflateState::wsize),
    /// [`whave`](InflateState::whave), and [`wnext`](InflateState::wnext) — so
    /// the previously decoded data is no longer used as history — and then
    /// performs the [`reset_keep`](InflateState::reset_keep) reset. The window
    /// *allocation* is retained (only its logical contents are dropped),
    /// matching C, which keeps `state->window` allocated.
    pub fn reset(&mut self) {
        self.wsize = 0;
        self.whave = 0;
        self.wnext = 0;
        self.reset_keep();
    }

    /// Clears the input bit accumulator.
    ///
    /// Sets [`hold`](InflateState::hold) and [`bits`](InflateState::bits) back
    /// to zero, mirroring the C `INITBITS()` macro used by the driver after a
    /// value has been fully consumed from the bit buffer.
    #[inline]
    pub fn clear_bitbuffer(&mut self) {
        self.hold = 0;
        self.bits = 0;
    }

    /// Returns whether this state is in a valid inflate mode.
    ///
    /// This mirrors the mode-range portion of the C `inflateStateCheck`
    /// predicate, which treats a state as valid when
    /// `HEAD <= mode <= SYNC`. Because [`InflateMode`] is a closed enum that can
    /// only ever hold one of its defined variants, Rust's type system already
    /// guarantees the mode lies in `[Head, Sync]` — so this always returns
    /// `true`. It is retained for parity with the C API and as an explicit,
    /// self-documenting hook the driver (`mod.rs`) can call where the C code
    /// invoked `inflateStateCheck`.
    #[must_use]
    #[inline]
    pub fn is_valid(&self) -> bool {
        let mode = self.mode as u16;
        (InflateMode::Head as u16..=InflateMode::Sync as u16).contains(&mode)
    }

    /// Resolves the active literal/length decode table to a slice.
    ///
    /// Pass the module-static fixed literal/length table
    /// (`&crate::inflate::fixed::LENFIX[..]`) as `fixed`. It is returned
    /// verbatim when [`lentable`](InflateState::lentable) is
    /// [`TableSource::Fixed`]; otherwise the sub-slice of
    /// [`codes`](InflateState::codes) beginning at
    /// [`lencode`](InflateState::lencode) is returned.
    ///
    /// Taking `fixed` as an argument (rather than importing the fixed tables)
    /// keeps this data-model module free of a dependency on the fixed-table
    /// module and free of any `unsafe` self-referential borrow: the returned
    /// slice borrows from either `self` or `fixed`, both tied to the `'a`
    /// lifetime.
    #[must_use]
    #[inline]
    pub fn lencode_slice<'a>(&'a self, fixed: &'a [Code]) -> &'a [Code] {
        match self.lentable {
            TableSource::Fixed => fixed,
            TableSource::Dynamic => &self.codes[self.lencode..],
        }
    }

    /// Resolves the active distance decode table to a slice.
    ///
    /// The distance counterpart of [`lencode_slice`](InflateState::lencode_slice):
    /// pass the module-static fixed distance table
    /// (`&crate::inflate::fixed::DISTFIX[..]`) as `fixed`. It is returned when
    /// [`disttable`](InflateState::disttable) is [`TableSource::Fixed`];
    /// otherwise the sub-slice of [`codes`](InflateState::codes) beginning at
    /// [`distcode`](InflateState::distcode) is returned.
    #[must_use]
    #[inline]
    pub fn distcode_slice<'a>(&'a self, fixed: &'a [Code]) -> &'a [Code] {
        match self.disttable {
            TableSource::Fixed => fixed,
            TableSource::Dynamic => &self.codes[self.distcode..],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds an owned window of `n` zero bytes as an [`AllocBuffer`] (the
    /// [`window`](InflateState::window) field type) using the global allocator,
    /// so the helper is valid in both `no_std` + `alloc` and `std` test builds
    /// and can be assigned directly to `state.window`.
    fn make_window(n: usize) -> AllocBuffer<u8> {
        // The null hook always yields an owned global-allocator buffer, so
        // `try_zeroed` never returns `None` here.
        AllocBuffer::try_zeroed(n, AllocHook::none()).expect("global allocation is infallible")
    }

    #[test]
    fn head_sentinel_value() {
        // The C `inflate_mode` enum starts at HEAD = 16180.
        assert_eq!(InflateMode::Head as u16, 16180);
    }

    #[test]
    fn sync_value_and_span() {
        // SYNC is the last mode; 32 contiguous variants => span of 31 from HEAD.
        assert_eq!(InflateMode::Sync as u16, 16211);
        assert_eq!(InflateMode::Sync as u16 - InflateMode::Head as u16, 31);
    }

    #[test]
    fn all_32_variants_are_contiguous() {
        // Every variant, listed in the exact C order, must be exactly one more
        // than the previous, and there must be exactly 32 of them.
        let variants = [
            InflateMode::Head,
            InflateMode::Flags,
            InflateMode::Time,
            InflateMode::Os,
            InflateMode::ExLen,
            InflateMode::Extra,
            InflateMode::Name,
            InflateMode::Comment,
            InflateMode::Hcrc,
            InflateMode::DictId,
            InflateMode::Dict,
            InflateMode::Type,
            InflateMode::TypeDo,
            InflateMode::Stored,
            InflateMode::CopyUnderscore,
            InflateMode::Copy,
            InflateMode::Table,
            InflateMode::LenLens,
            InflateMode::CodeLens,
            InflateMode::LenUnderscore,
            InflateMode::Len,
            InflateMode::LenExt,
            InflateMode::Dist,
            InflateMode::DistExt,
            InflateMode::Match,
            InflateMode::Lit,
            InflateMode::Check,
            InflateMode::Length,
            InflateMode::Done,
            InflateMode::Bad,
            InflateMode::Mem,
            InflateMode::Sync,
        ];
        assert_eq!(variants.len(), 32);
        for (i, mode) in variants.iter().enumerate() {
            assert_eq!(*mode as u16, 16180 + i as u16);
        }
    }

    #[test]
    fn mode_default_is_head() {
        assert_eq!(InflateMode::default(), InflateMode::Head);
    }

    #[test]
    fn table_source_default_is_dynamic() {
        assert_eq!(TableSource::default(), TableSource::Dynamic);
    }

    #[test]
    fn codes_arena_is_enough_1444() {
        let state = InflateState::new(0, 15);
        assert_eq!(ENOUGH, 1444);
        assert_eq!(state.codes.len(), ENOUGH);
        assert_eq!(state.codes.len(), 1444);
    }

    #[test]
    fn scratch_arrays_sized_exactly() {
        let state = InflateState::new(0, 15);
        assert_eq!(state.lens.len(), 320);
        assert_eq!(state.work.len(), 288);
    }

    #[test]
    fn new_reproduces_c_initial_state() {
        let state = InflateState::new(1, 15);
        assert_eq!(state.mode, InflateMode::Head);
        assert!(state.sane);
        assert_eq!(state.back, -1);
        assert!(state.window.is_empty());
        assert_eq!(state.wrap, 1);
        assert_eq!(state.wbits, 15);
        assert_eq!(state.flags, -1);
        assert_eq!(state.dmax, 32768);
        assert_eq!(state.total, 0);
        assert!(state.head.is_none());
        assert!(!state.last);
        assert!(!state.havedict);
        assert_eq!(state.hold, 0);
        assert_eq!(state.bits, 0);
        assert_eq!(state.lencode, 0);
        assert_eq!(state.distcode, 0);
        assert_eq!(state.next, 0);
        assert_eq!(state.lentable, TableSource::Dynamic);
        assert_eq!(state.disttable, TableSource::Dynamic);
        assert_eq!(state.wsize, 0);
        assert_eq!(state.whave, 0);
        assert_eq!(state.wnext, 0);
        assert_eq!(state.codes[0], Code::default());
    }

    #[test]
    fn is_valid_is_true_for_every_mode() {
        // The closed enum can only hold valid modes, so is_valid is always true;
        // check a representative set including the terminal error modes.
        let mut state = InflateState::new(0, 15);
        for mode in [
            InflateMode::Head,
            InflateMode::Len,
            InflateMode::Bad,
            InflateMode::Mem,
            InflateMode::Sync,
        ] {
            state.mode = mode;
            assert!(state.is_valid());
        }
    }

    #[test]
    fn clear_bitbuffer_zeroes_accumulator() {
        let mut state = InflateState::new(0, 15);
        state.hold = 0xDEAD_BEEF;
        state.bits = 24;
        state.clear_bitbuffer();
        assert_eq!(state.hold, 0);
        assert_eq!(state.bits, 0);
    }

    #[test]
    fn reset_keep_restores_reset_fields_but_keeps_window() {
        let mut state = InflateState::new(1, 15);
        // Simulate arbitrary mid-stream mutation.
        state.mode = InflateMode::Len;
        state.last = true;
        state.havedict = true;
        state.flags = 0;
        state.hold = 4321;
        state.bits = 15;
        state.back = 7;
        state.sane = false;
        state.lencode = 42;
        state.distcode = 99;
        state.next = 100;
        state.lentable = TableSource::Fixed;
        state.disttable = TableSource::Fixed;
        state.total = 999;
        state.dmax = 4096;
        // Pretend the window is in use.
        state.window = make_window(8);
        state.wsize = 8;
        state.whave = 4;
        state.wnext = 2;

        state.reset_keep();

        // Reset-managed fields are restored to their initial values.
        assert_eq!(state.mode, InflateMode::Head);
        assert!(!state.last);
        assert!(!state.havedict);
        assert_eq!(state.flags, -1);
        assert_eq!(state.hold, 0);
        assert_eq!(state.bits, 0);
        assert_eq!(state.back, -1);
        assert!(state.sane);
        assert_eq!(state.lencode, 0);
        assert_eq!(state.distcode, 0);
        assert_eq!(state.next, 0);
        assert_eq!(state.lentable, TableSource::Dynamic);
        assert_eq!(state.disttable, TableSource::Dynamic);
        assert_eq!(state.total, 0);
        assert_eq!(state.dmax, 32768);

        // The window allocation and geometry are kept (this is "reset *keep*").
        assert_eq!(state.window.len(), 8);
        assert_eq!(state.wsize, 8);
        assert_eq!(state.whave, 4);
        assert_eq!(state.wnext, 2);
    }

    #[test]
    fn reset_invalidates_window_geometry_but_keeps_allocation() {
        let mut state = InflateState::new(1, 15);
        state.window = make_window(8);
        state.wsize = 8;
        state.whave = 4;
        state.wnext = 2;
        state.mode = InflateMode::Len;

        state.reset();

        // Window geometry is zeroed (history invalidated) ...
        assert_eq!(state.wsize, 0);
        assert_eq!(state.whave, 0);
        assert_eq!(state.wnext, 0);
        // ... but the allocation is retained, matching C keeping state->window.
        assert_eq!(state.window.len(), 8);
        // ... and the reset_keep fields are applied too.
        assert_eq!(state.mode, InflateMode::Head);
        assert_eq!(state.back, -1);
    }

    #[test]
    fn lencode_slice_dynamic_indexes_into_codes() {
        let mut state = InflateState::new(0, 15);
        state.lentable = TableSource::Dynamic;
        state.lencode = 3;
        state.codes[3] = Code {
            op: 1,
            bits: 2,
            val: 7,
        };
        let fixed = [Code::default()];
        let slice = state.lencode_slice(&fixed);
        assert_eq!(
            slice[0],
            Code {
                op: 1,
                bits: 2,
                val: 7
            }
        );
        assert_eq!(slice.len(), ENOUGH - 3);
    }

    #[test]
    fn lencode_slice_fixed_returns_the_fixed_table() {
        let mut state = InflateState::new(0, 15);
        state.lentable = TableSource::Fixed;
        let fixed = [
            Code {
                op: 9,
                bits: 9,
                val: 99,
            },
            Code::default(),
        ];
        let slice = state.lencode_slice(&fixed);
        assert_eq!(slice.len(), 2);
        assert_eq!(
            slice[0],
            Code {
                op: 9,
                bits: 9,
                val: 99
            }
        );
    }

    #[test]
    fn distcode_slice_dynamic_and_fixed() {
        let mut state = InflateState::new(0, 15);
        state.disttable = TableSource::Dynamic;
        state.distcode = 5;
        state.codes[5] = Code {
            op: 4,
            bits: 5,
            val: 6,
        };
        let fixed = [Code {
            op: 1,
            bits: 1,
            val: 1,
        }];
        assert_eq!(
            state.distcode_slice(&fixed)[0],
            Code {
                op: 4,
                bits: 5,
                val: 6
            }
        );

        state.disttable = TableSource::Fixed;
        assert_eq!(
            state.distcode_slice(&fixed)[0],
            Code {
                op: 1,
                bits: 1,
                val: 1
            }
        );
    }

    #[test]
    fn state_is_boxable_matching_stream_model() {
        // `new` already yields a `Box<InflateState>`; inspect it before moving
        // it into the `Option<Box<InflateState>>` shape held by `src/stream.rs`.
        let state: Box<InflateState> = InflateState::new(2, 15);
        assert_eq!(state.mode, InflateMode::Head);
        assert_eq!(state.wrap, 2);
        let boxed: Option<Box<InflateState>> = Some(state);
        assert!(boxed.is_some());
    }
}
