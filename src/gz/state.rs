//! Internal gzip file state (`GzState`) and shared sentinels, ported from zlib
//! `gzguts.h`. Backs the `gz*` file-I/O API. RAII `Drop` replaces `gzclose`.
//!
//! This module is the **foundational model** of the [`crate::gz`] file-I/O
//! layer: every other `gz` module (`open`, `read`, `write`, `close`) threads a
//! `&mut `[`GzState`] through its functions, exactly as the C `gz*` functions
//! thread a `gz_statep` (`gzguts.h`, the `gz_state` struct). It is a faithful,
//! **100% safe** Rust port of:
//!
//! * the C `gz_state` structure (`gzguts.h`) and its embedded `struct gzFile_s`
//!   (`zlib.h`) — the "exposed contents" used by the `gzgetc()` macro;
//! * the mode sentinels `GZ_NONE` / `GZ_READ` / `GZ_WRITE` / `GZ_APPEND`;
//! * the read-substate `how` values `LOOK` / `COPY` / `GZIP`;
//! * the default i/o buffer size `GZBUFSIZE`;
//! * the shared `gz_error` routine (`gzlib.c`) and the `gz_intmax` helper.
//!
//! # The idiomatic twin of `gz_state`
//!
//! The C structure reaches everything through raw pointers and an embedded,
//! in-place `z_stream`. The Rust port replaces each unsafe C idiom with an
//! owned, safe equivalent (AAP §0.6.3, the memory-ownership model):
//!
//! | C `gz_state` idiom                       | `GzState` equivalent                      |
//! |------------------------------------------|-------------------------------------------|
//! | `malloc`'d `in` / `out` byte buffers     | owned `Vec<u8>` ([`in_buf`](GzState)/[`out_buf`](GzState)) |
//! | raw `fd` (`int`)                         | owned [`std::fs::File`] ([`file`](GzState), `Option`) |
//! | `x.next` raw pointer into `out`          | integer index [`next`](GzState) into `out_buf` |
//! | `strm.next_in` raw pointer into `in`     | integer index [`in_next`](GzState) into `in_buf` |
//! | pointer-plus-length (`x.have`, `avail_*`)| length counters [`have`](GzState) / [`in_avail`](GzState) |
//! | embedded `z_stream strm`                 | owned [`ZStream`] ([`strm`](GzState))     |
//! | manual `gzclose` teardown                | [`Drop`] (RAII — deterministic, leak-free) |
//!
//! # Cursor-vs-pointer mapping (consumed by `read.rs` / `write.rs`)
//!
//! C uses raw pointers that walk along the buffers; Rust uses **integer
//! indices** into the owned `Vec<u8>`s plus length counters:
//!
//! * Output side: [`have`](GzState) bytes of decompressed / to-be-written data
//!   are available starting at index [`next`](GzState) within
//!   [`out_buf`](GzState). The `gzgetc` fast path delivers `out_buf[next]`,
//!   advancing `next` and decrementing `have` (see [`out_slice`](GzState::out_slice)).
//! * Input side: [`in_avail`](GzState) unread input bytes begin at index
//!   [`in_next`](GzState) within [`in_buf`](GzState). The compression /
//!   decompression engine is handed a `&[u8]` slice of `in_buf` per call rather
//!   than a stored pointer.
//!
//! The engine's own `next_out` / `avail_out` are **not** stored here: each
//! `deflate` / `inflate` call receives a `&mut [u8]` slice carved from
//! [`out_buf`](GzState) (see [`ZStream`] for the lifetime-free engine design).
//!
//! # Feature gating
//!
//! The whole `gz` folder is gated by the `gz-io` Cargo feature (which implies
//! `std` + `gzip`); the gating lives at the `pub mod gz` declaration in
//! `lib.rs`, so this module may freely use `std` (`std::fs::File`,
//! `std::string::String`, heap `Vec`). There is intentionally **no**
//! `#![cfg(...)]` at the top of this file.
//!
//! # No `unsafe`
//!
//! This module contains **no `unsafe`** (AAP §0.6.2 confines `unsafe` to
//! `src/ffi.rs` and `src/inflate/fast.rs`). C-string / `CStr` marshalling is
//! the FFI layer's concern, not this module's.

use std::fs::File;

use crate::constants::{Z_BUF_ERROR, Z_DEFAULT_COMPRESSION, Z_DEFAULT_STRATEGY, Z_MEM_ERROR, Z_OK};
use crate::stream::ZStream;

// ===========================================================================
// GZBUFSIZE — default i/o buffer size (gzguts.h L156)
// ===========================================================================

/// Default i/o buffer size (C `GZBUFSIZE`).
///
/// The output buffer is double this when reading, and the input buffer is
/// double this when writing — this value and twice this value must both fit in
/// the buffer-size type. A caller may request a different size via `gzbuffer`
/// before the first read/write (it lands in [`GzState::want`]); until the
/// buffers are first allocated, [`GzState::size`] is `0`.
pub const GZBUFSIZE: usize = 8192;

// ===========================================================================
// Mode — gzip stream modes (gzguts.h L159-162)
// ===========================================================================
//
// These integer values are FROZEN: they must equal the C `#define`s exactly,
// because they double as a lightweight integrity check on the handle passed
// across the FFI boundary (a wild pointer is overwhelmingly unlikely to carry
// one of these arbitrary sentinels in its `mode` field) and are read / written
// as raw `int`s by the FFI shim. See AAP §0.6.1 / §0.7.1.

/// Raw `mode` sentinel: the stream is not open (C `GZ_NONE`).
pub const GZ_NONE: i32 = 0;

/// Raw `mode` sentinel: append mode (C `GZ_APPEND`).
///
/// Transient — after a successful open in append mode the mode is rewritten to
/// [`GZ_WRITE`] (see [`Mode::Append`]).
pub const GZ_APPEND: i32 = 1;

/// Raw `mode` sentinel: the stream is open for reading (C `GZ_READ`).
pub const GZ_READ: i32 = 7247;

/// Raw `mode` sentinel: the stream is open for writing (C `GZ_WRITE`).
pub const GZ_WRITE: i32 = 31153;

/// gzip stream mode — the idiomatic form of the C `GZ_*` mode sentinels.
///
/// The `#[repr(i32)]` discriminants are **frozen** to the exact C `#define`
/// values ([`GZ_NONE`] `= 0`, [`GZ_APPEND`] `= 1`, [`GZ_READ`] `= 7247`,
/// [`GZ_WRITE`] `= 31153`) so that the enum and the raw FFI `int` are
/// interchangeable via [`as_i32`](Mode::as_i32) / [`from_i32`](Mode::from_i32).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum Mode {
    /// Not open (C `GZ_NONE`). Also used as the "already finalized" marker once
    /// an explicit `close()` has run, so [`Drop`] becomes a no-op.
    None = GZ_NONE,
    /// Append (C `GZ_APPEND`). **Transient**: `gz_open` seeks to end-of-file and
    /// then rewrites the mode to [`Mode::Write`] (matching C `gz_open`, which
    /// sets `state->mode = GZ_WRITE` after `lseek(SEEK_END)`), so a steady-state
    /// stream is never observed in this mode.
    Append = GZ_APPEND,
    /// Open for reading (C `GZ_READ`).
    Read = GZ_READ,
    /// Open for writing (C `GZ_WRITE`).
    Write = GZ_WRITE,
}

impl Mode {
    /// Return the raw C `int` value of this mode (its `#[repr(i32)]`
    /// discriminant), for the FFI shim and the handle integrity check.
    ///
    /// # Examples
    ///
    /// ```
    /// use zlib_rs::gz::state::Mode;
    /// assert_eq!(Mode::Read.as_i32(), 7247);
    /// assert_eq!(Mode::Write.as_i32(), 31153);
    /// ```
    #[inline]
    #[must_use]
    pub const fn as_i32(self) -> i32 {
        self as i32
    }

    /// Parse a raw C `int` mode value into a [`Mode`].
    ///
    /// Returns `Some` only for the four frozen sentinels; any other value
    /// yields `None` (which the FFI layer treats as a corrupt / non-gz handle).
    ///
    /// # Examples
    ///
    /// ```
    /// use zlib_rs::gz::state::Mode;
    /// assert_eq!(Mode::from_i32(7247), Some(Mode::Read));
    /// assert_eq!(Mode::from_i32(31153), Some(Mode::Write));
    /// assert_eq!(Mode::from_i32(12345), None);
    /// ```
    #[inline]
    #[must_use]
    pub fn from_i32(v: i32) -> Option<Mode> {
        match v {
            GZ_NONE => Some(Mode::None),
            GZ_APPEND => Some(Mode::Append),
            GZ_READ => Some(Mode::Read),
            GZ_WRITE => Some(Mode::Write),
            _ => None,
        }
    }
}

// ===========================================================================
// How — read-path substate (gzguts.h L165-167)
// ===========================================================================
//
// Frozen sentinels for the `gz_state.how` field. Used only on the read path.

/// Raw `how` sentinel: look for a gzip header (C `LOOK`).
pub const LOOK: i32 = 0;

/// Raw `how` sentinel: copy input directly, transparently (C `COPY`).
pub const COPY: i32 = 1;

/// Raw `how` sentinel: decompress a gzip stream (C `GZIP`).
pub const GZIP: i32 = 2;

/// Read-path substate — the idiomatic form of the C `how` values.
///
/// Tracks what the reader is currently doing with the input stream. The
/// `#[repr(i32)]` discriminants equal the C `#define`s ([`LOOK`] `= 0`,
/// [`COPY`] `= 1`, [`GZIP`] `= 2`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum How {
    /// Look for a gzip header at the current position (C `LOOK`). The initial
    /// substate of a freshly reset read stream.
    Look = LOOK,
    /// Copy input directly to output (transparent / non-gzip data) (C `COPY`).
    Copy = COPY,
    /// Decompress a gzip stream (C `GZIP`).
    Gzip = GZIP,
}

impl How {
    /// Return the raw C `int` value of this substate (its `#[repr(i32)]`
    /// discriminant).
    ///
    /// # Examples
    ///
    /// ```
    /// use zlib_rs::gz::state::How;
    /// assert_eq!(How::Look.as_i32(), 0);
    /// assert_eq!(How::Gzip.as_i32(), 2);
    /// ```
    #[inline]
    #[must_use]
    pub const fn as_i32(self) -> i32 {
        self as i32
    }
}

// ===========================================================================
// FinalizeFn — the write-path teardown hook (decouples Drop from `write.rs`)
// ===========================================================================

/// A best-effort finalize routine installed by the write path so that
/// [`Drop`] can flush a still-open write stream without `state.rs` taking a
/// compile-time dependency on `write.rs`.
///
/// The C library performs the final `deflate(…, Z_FINISH)` flush and
/// `deflateEnd` inside `gzclose_w` (`gzwrite.c`). In the Rust port that logic
/// lives in [`crate::gz::write`]; to honour the AAP §0.3.2 / §0.6.3 RAII
/// contract — *dropping a handle must never leak and must flush a write stream
/// on a best-effort basis* — the write module installs its finalize routine
/// here via [`GzState::set_finalize`]. [`Drop`] then invokes it (discarding the
/// returned `Z_*` code, since a destructor cannot return a `Result`), and the
/// explicit [`crate::gz::close`] path consumes it via
/// [`GzState::take_finalize`] so that a subsequent drop is a clean no-op (the
/// double-finalize guard).
///
/// Keeping this a plain function pointer (rather than a boxed closure) is
/// sufficient because the finalize routine operates entirely on the
/// `&mut GzState` it is handed — the engine ([`strm`](GzState::strm)), the
/// buffers, and the [`file`](GzState::file) are all reachable from the state
/// itself. The installed routine **must be panic-free** so that [`Drop`] never
/// unwinds.
pub(crate) type FinalizeFn = fn(&mut GzState) -> i32;

// ===========================================================================
// GzState — the internal gzip file state (gzguts.h `gz_state`, L170-204)
// ===========================================================================

/// Internal gzip file state — the safe-Rust port of the C `gz_state` struct
/// (`gzguts.h`).
///
/// `GzState` is the single handle threaded through the entire `gz*` file-I/O
/// API. A `gzFile` at the FFI boundary is a pointer to one of these; the
/// idiomatic Rust API owns it directly. All working storage is owned (heap
/// `Vec<u8>` buffers, an owned [`File`], an owned [`ZStream`] engine), so
/// teardown is the deterministic, leak-free [`Drop`] that replaces `gzclose`
/// (see the [module documentation](self) for the full C-to-Rust field mapping).
///
/// The fields are grouped exactly as in the C source for traceability, and are
/// `pub(crate)` so the sibling `open` / `read` / `write` / `close` modules can
/// manipulate them while the type stays opaque to external callers.
pub struct GzState {
    // --- exposed contents (C: `struct gzFile_s x`, used by the gzgetc macro) ---
    /// Number of output bytes available at [`next`](Self::next) within
    /// [`out_buf`](Self::out_buf) (C `x.have`).
    pub(crate) have: usize,
    /// Index of the next output byte to deliver / write, **within**
    /// [`out_buf`](Self::out_buf) (C `x.next`, which is a raw pointer; here an
    /// integer cursor — see the [module docs](self)).
    pub(crate) next: usize,
    /// Current position in the uncompressed data stream (C `x.pos`,
    /// `z_off64_t`). Signed `i64` to mirror the C 64-bit signed offset.
    pub(crate) pos: i64,

    // --- used for both reading and writing ---
    /// Stream mode (C `mode`); see [`Mode`].
    pub(crate) mode: Mode,
    /// The underlying OS file, owned (C `fd`, a raw descriptor). `None` once the
    /// stream has been explicitly closed, after which [`Drop`] is a no-op for
    /// the file (it is already closed).
    pub(crate) file: Option<File>,
    /// Path (or `fd` identifier) used to prefix error messages (C `path`).
    pub(crate) path: String,
    /// Allocated buffer size; `0` if the buffers have not been allocated yet
    /// (C `size`). `size == 0` ⇔ [`in_buf`](Self::in_buf) /
    /// [`out_buf`](Self::out_buf) are still empty, exactly as in C.
    pub(crate) size: usize,
    /// Requested buffer size, default [`GZBUFSIZE`] (C `want`). Settable via
    /// `gzbuffer` before the first i/o.
    pub(crate) want: usize,
    /// Input buffer (C `in`). Owned; sized `2 * want` when writing. Empty until
    /// first allocated (`size == 0`).
    pub(crate) in_buf: Vec<u8>,
    /// Output buffer (C `out`). Owned; sized `2 * want` when reading. Empty
    /// until first allocated (`size == 0`).
    pub(crate) out_buf: Vec<u8>,
    /// Transparency tri-state (C `direct`): `0` processing gzip, `1`
    /// transparent (copy input straight through), `-1` gzip-only (the read-side
    /// `'G'` mode that forbids transparent fallback). Kept as a raw [`i32`] so
    /// the exact `-1` / `0` / `1` comparisons in `open.rs` / `read.rs` are
    /// reproduced faithfully.
    pub(crate) direct: i32,

    // --- just for reading ---
    /// gzip-member scan marker (C `junk`): `-1` at the very start, `1` while a
    /// trailing-junk candidate is being examined, `0` once inside a gzip member.
    pub(crate) junk: i32,
    /// Read-path substate (C `how`); see [`How`].
    pub(crate) how: How,
    /// `true` if the last i/o returned `EAGAIN` / `EWOULDBLOCK` (C `again`).
    /// While set, [`gz_error`](Self::gz_error) does **not** treat an error as
    /// fatal (it leaves [`have`](Self::have) intact for a retry).
    pub(crate) again: bool,
    /// Offset at which the gzip data started, used for rewinding (C `start`,
    /// `z_off64_t`).
    pub(crate) start: i64,
    /// `true` once the end of the input file has been reached (C `eof`).
    pub(crate) eof: bool,
    /// `true` if a read was requested past the end of the data (C `past`).
    pub(crate) past: bool,

    // --- just for writing ---
    /// Compression level (C `level`); default [`Z_DEFAULT_COMPRESSION`].
    pub(crate) level: i32,
    /// Compression strategy (C `strategy`); default [`Z_DEFAULT_STRATEGY`].
    pub(crate) strategy: i32,
    /// `true` if a `deflateReset` is pending after a `Z_FINISH` (C `reset`).
    pub(crate) reset: bool,

    // --- seek request ---
    /// Amount left to skip for a pending forward seek; already rewound if the
    /// seek was backwards (C `skip`, `z_off64_t`).
    pub(crate) skip: i64,

    // --- error information ---
    /// Last error code, held as a raw `Z_*` value (C `err`): [`Z_OK`],
    /// [`Z_BUF_ERROR`], `Z_ERRNO`, `Z_DATA_ERROR`, [`Z_MEM_ERROR`],
    /// `Z_STREAM_ERROR`. Kept as [`i32`] so `gzerror` / `gzclose_*` return
    /// values are bit-exact with C; conversion to
    /// [`ReturnCode`](crate::error::ReturnCode) /
    /// [`ZlibError`](crate::error::ZlibError) is for ergonomics only.
    pub(crate) err: i32,
    /// Last formatted error message `"<path>: <message>"`, or `None` (C `msg`).
    /// `None` for [`Z_MEM_ERROR`] — `gzerror` reports the literal
    /// `"out of memory"` for that code rather than storing an allocated string
    /// (see [`error_message`](Self::error_message)).
    pub(crate) msg: Option<String>,

    // --- zlib inflate or deflate stream (C: `z_stream strm` in-place) ---
    /// The owned compression / decompression engine (C `strm`, an in-place
    /// `z_stream`). Owning it here means its own [`Drop`] frees the deflate /
    /// inflate state when the `GzState` is dropped (RAII; AAP §0.6.3).
    pub(crate) strm: ZStream,
    /// Index into [`in_buf`](Self::in_buf) of the next unread input byte
    /// (mirrors the engine's `next_in` pointer; here an integer cursor).
    pub(crate) in_next: usize,
    /// Number of valid input bytes available at [`in_next`](Self::in_next)
    /// (mirrors the engine's `avail_in`).
    pub(crate) in_avail: usize,

    /// The owned **deflate** engine used by the write path (`write.rs`).
    ///
    /// The generic [`strm`](Self::strm) holds a `Box<dyn StreamState>`, which
    /// cannot be recovered as a concrete `&mut DeflateState` (the
    /// [`StreamState`](crate::stream::StreamState) trait exposes no downcast).
    /// Because the gzip write path must call [`deflate`](crate::deflate::deflate)
    /// — which operates on a concrete `DeflateState` — the engine is stored here
    /// as an owned `Box<DeflateState>` (mirroring the canonical `Deflate`
    /// consumer in `util/compress.rs`). It is `None` until the first write
    /// lazily initializes it (C `gz_init`'s `deflateInit2`), and its own
    /// [`Drop`] frees the deflate state when the `GzState` is dropped (RAII;
    /// AAP §0.6.3). Read streams leave this `None` and use the generic engine.
    pub(crate) deflate: Option<Box<crate::deflate::DeflateState>>,

    // --- teardown hook (no C analogue; realises the RAII Drop contract) ---
    /// Optional best-effort finalize routine installed by the write path; see
    /// [`FinalizeFn`]. `Some` for an active write stream, `None` otherwise and
    /// after an explicit close (the double-finalize guard).
    pub(crate) finalize: Option<FinalizeFn>,
}

impl GzState {
    /// Create a freshly opened `GzState`, initialised to the C `gz_open`
    /// post-allocation defaults.
    ///
    /// This mirrors the field initialisation performed by C `gz_open`
    /// (`gzlib.c`) immediately after it `malloc`s the `gz_state` and before it
    /// parses the mode string: the buffer size is unallocated
    /// ([`size`](Self::size) `= 0`), the requested size is [`GZBUFSIZE`], the
    /// level / strategy take their `Z_DEFAULT_*` values, and the error state is
    /// clear. The read/write-specific fields are seeded to the values C
    /// `gz_reset` (`gzlib.c`) would set on a fresh stream: cursors and counters
    /// are `0`, the flags are `false`, [`junk`](Self::junk) is `-1` (the
    /// "first member" marker), and [`how`](Self::how) is [`How::Look`]. The
    /// caller (`open.rs`) performs the precise mode-specific reset afterwards.
    ///
    /// The supplied [`File`] is taken by value and owned for the stream's
    /// lifetime; `path` is retained verbatim for error-message prefixing.
    #[must_use]
    pub(crate) fn new(file: File, path: String, mode: Mode) -> GzState {
        GzState {
            // exposed contents
            have: 0,
            next: 0,
            pos: 0,
            // shared
            mode,
            file: Some(file),
            path,
            size: 0,
            want: GZBUFSIZE,
            in_buf: Vec::new(),
            out_buf: Vec::new(),
            direct: 0,
            // reading
            junk: -1,
            how: How::Look,
            again: false,
            start: 0,
            eof: false,
            past: false,
            // writing
            level: Z_DEFAULT_COMPRESSION,
            strategy: Z_DEFAULT_STRATEGY,
            reset: false,
            // seek
            skip: 0,
            // error
            err: Z_OK,
            msg: None,
            // engine
            strm: ZStream::new(),
            in_next: 0,
            in_avail: 0,
            deflate: None,
            // teardown hook
            finalize: None,
        }
    }

    // -----------------------------------------------------------------------
    // Buffer / mode accessors used by the sibling gz modules.
    // -----------------------------------------------------------------------

    /// The currently available output bytes: `&out_buf[next .. next + have]`.
    ///
    /// This is the data the `gzgetc` / `gzread` fast path delivers without
    /// touching the engine. Returns an empty slice when [`have`](Self::have) is
    /// `0`.
    #[inline]
    #[must_use]
    pub(crate) fn out_slice(&self) -> &[u8] {
        &self.out_buf[self.next..self.next + self.have]
    }

    /// `true` if the stream is open for reading ([`Mode::Read`]).
    #[inline]
    #[must_use]
    pub(crate) fn is_reading(&self) -> bool {
        self.mode == Mode::Read
    }

    /// `true` if the stream is open for writing ([`Mode::Write`]).
    ///
    /// Note that [`Mode::Append`] is transient — `gz_open` rewrites it to
    /// [`Mode::Write`] — so a steady-state writable stream always reports
    /// `Mode::Write` here.
    #[inline]
    #[must_use]
    pub(crate) fn is_writing(&self) -> bool {
        self.mode == Mode::Write
    }

    // -----------------------------------------------------------------------
    // Teardown-hook management (drives the RAII Drop contract).
    // -----------------------------------------------------------------------

    /// Install the best-effort write-finalize routine (see [`FinalizeFn`]).
    ///
    /// Called by the write path (`write.rs`, in its `gz_init` equivalent) so a
    /// dropped-but-not-explicitly-closed write stream still flushes on a
    /// best-effort basis. The routine **must be panic-free**.
    #[inline]
    pub(crate) fn set_finalize(&mut self, finalize: FinalizeFn) {
        self.finalize = Some(finalize);
    }

    /// Remove and return the installed finalize routine, if any.
    ///
    /// The explicit close path (`close.rs`) calls this so it can run the
    /// finalize *itself* (observing the returned `Z_*` code), after which the
    /// eventual [`Drop`] sees `finalize == None` and does nothing — the
    /// double-finalize guard.
    #[inline]
    pub(crate) fn take_finalize(&mut self) -> Option<FinalizeFn> {
        self.finalize.take()
    }

    /// Mark the stream as fully closed so a subsequent [`Drop`] is a no-op.
    ///
    /// Sets [`mode`](Self::mode) to [`Mode::None`] and clears the
    /// [`finalize`](Self::finalize) hook. The explicit `gzclose*` path calls
    /// this after it has finished and torn the engine down, so that dropping
    /// the (now-inert) handle neither re-finalizes nor errors. The owned
    /// [`file`](Self::file) and buffers are still released by their own `Drop`.
    #[inline]
    pub(crate) fn mark_closed(&mut self) {
        self.mode = Mode::None;
        self.finalize = None;
    }

    // -----------------------------------------------------------------------
    // gz_error — error recording (gzlib.c `gz_error`, L555-590).
    // -----------------------------------------------------------------------

    /// Record an error on the stream, exactly mirroring C `gz_error`
    /// (`gzlib.c`).
    ///
    /// Behaviour, step for step with the C source:
    ///
    /// 1. Any previously stored [`msg`](Self::msg) is cleared.
    /// 2. If the error is **fatal** — `err` is neither [`Z_OK`] nor
    ///    [`Z_BUF_ERROR`], and [`again`](Self::again) is `false` —
    ///    [`have`](Self::have) is zeroed so the `gzgetc` fast path falls through
    ///    to the slow path and surfaces the error.
    /// 3. The error code is stored in [`err`](Self::err).
    /// 4. If `msg` is `None`, recording is complete.
    /// 5. If `err` is [`Z_MEM_ERROR`], no message is allocated — `gzerror`
    ///    reports the literal `"out of memory"` for that code (see
    ///    [`error_message`](Self::error_message)), so [`msg`](Self::msg) stays
    ///    `None`.
    /// 6. Otherwise the message is formatted as `"<path>: <message>"`.
    ///
    /// Unlike C — which `malloc`s the message string and can itself fail with
    /// `Z_MEM_ERROR` — the Rust port builds an owned [`String`]; on the rare
    /// host allocation failure the global allocator aborts rather than
    /// unwinding, so the C "allocation failed while formatting" fallback path
    /// has no observable analogue here.
    pub(crate) fn gz_error(&mut self, err: i32, msg: Option<&str>) {
        // (1) free / clear any previous message
        self.msg = None;

        // (2) if fatal, zero `have` so the fast gzgetc path fails over
        if err != Z_OK && err != Z_BUF_ERROR && !self.again {
            self.have = 0;
        }

        // (3) record the error code
        self.err = err;

        // (4) no message requested -> done
        let Some(msg) = msg else {
            return;
        };

        // (5) out-of-memory is reported as a literal string, never allocated
        if err == Z_MEM_ERROR {
            return;
        }

        // (6) construct "<path>: <message>"
        self.msg = Some(format!("{}: {}", self.path, msg));
    }

    /// Clear the error state — the idiomatic spelling of the very common C
    /// `gz_error(state, Z_OK, NULL)` call.
    #[inline]
    pub(crate) fn clear_error(&mut self) {
        self.gz_error(Z_OK, None);
    }

    /// The human-readable error message, exactly as C `gzerror` (`gzlib.c`)
    /// would return it.
    ///
    /// Returns the literal `"out of memory"` when [`err`](Self::err) is
    /// [`Z_MEM_ERROR`], the empty string when no message is stored, and the
    /// formatted `"<path>: <message>"` otherwise. The FFI `gzerror` shim turns
    /// this into a NUL-terminated C string.
    #[inline]
    #[must_use]
    pub(crate) fn error_message(&self) -> &str {
        if self.err == Z_MEM_ERROR {
            "out of memory"
        } else {
            self.msg.as_deref().unwrap_or("")
        }
    }

    /// Portable maximum `int` value (C `gz_intmax`, `gzlib.c`).
    ///
    /// C uses this to guard unsigned-vs-`z_off64_t` comparisons in its
    /// `GT_OFF(x)` macro. In this port positions and skip amounts are kept as
    /// signed [`i64`] (`z_off64_t`) and buffer sizes as [`usize`] throughout,
    /// so the `GT_OFF` overflow checks collapse and this helper is needed only
    /// by the FFI `gzseek` path that still speaks the raw C `int` width.
    #[inline]
    #[must_use]
    pub(crate) const fn gz_intmax() -> u32 {
        i32::MAX as u32
    }
}

// ===========================================================================
// Drop — RAII teardown replacing `gzclose` (AAP §0.3.2, §0.6.3)
// ===========================================================================

impl Drop for GzState {
    /// Deterministic, leak-free, panic-free teardown — the RAII replacement for
    /// the C `gzclose` family.
    ///
    /// Dropping a `GzState` must never leak and must never unwind. Both
    /// guarantees hold unconditionally: the owned [`ZStream`] frees the
    /// deflate / inflate engine state, and the owned [`File`] and the two
    /// `Vec<u8>` buffers close / free themselves via their own `Drop` once
    /// this function returns.
    ///
    /// On top of that guaranteed cleanup, an **initialized, non-transparent
    /// write** stream ([`Mode::Write`], [`size`](GzState::size) `!= 0`,
    /// [`direct`](GzState::direct) `== 0`) that has not already been finalized
    /// gets a *best-effort* final flush via the [`FinalizeFn`] the write path
    /// installed (mirroring the `deflate(Z_FINISH)` + `deflateEnd` that
    /// `gzclose_w` performs). The returned `Z_*` code is discarded — a
    /// destructor cannot surface a result; callers who must observe close-time
    /// I/O errors should use the explicit `close()` / `finish()` entry points
    /// (in `close.rs` / `write.rs`), which run the same routine and **return**
    /// the code. `Drop` is the safety net, not the primary close path (the same
    /// split `flate2`'s `GzEncoder::finish` + `Drop` uses).
    ///
    /// The flush runs **at most once**: taking the hook here, and the explicit
    /// close path's use of [`take_finalize`](GzState::take_finalize) /
    /// [`mark_closed`](GzState::mark_closed), ensure a handle is never
    /// finalized twice.
    fn drop(&mut self) {
        // Skip the optional flush for everything that does not need one: read
        // streams, uninitialized streams, transparent (`direct != 0`) writes,
        // and handles already closed (`mode` reset to `None`). The owned
        // fields are still dropped automatically after this early return.
        if self.mode != Mode::Write || self.size == 0 || self.direct != 0 {
            return;
        }

        // Best-effort flush. The installed routine is contractually panic-free,
        // and taking it makes this idempotent, so `Drop` never unwinds and
        // never double-finalizes.
        if let Some(finalize) = self.finalize.take() {
            let _ = finalize(self);
        }
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::constants::{
        Z_BUF_ERROR, Z_DATA_ERROR, Z_DEFAULT_COMPRESSION, Z_DEFAULT_STRATEGY, Z_MEM_ERROR, Z_OK,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A throwaway, always-available backing file for state construction. The
    /// tests here never inspect the file's contents, so `/dev/null` (present on
    /// the Linux test host) is ideal: writable, parallel-safe, no cleanup.
    fn dummy_file() -> File {
        File::options()
            .read(true)
            .write(true)
            .open("/dev/null")
            .expect("open /dev/null")
    }

    // --- frozen sentinels (AAP §0.6.1, §0.7.1) ---

    #[test]
    fn mode_sentinels_are_frozen() {
        assert_eq!(Mode::None as i32, 0);
        assert_eq!(Mode::Append as i32, 1);
        assert_eq!(Mode::Read as i32, 7247);
        assert_eq!(Mode::Write as i32, 31153);
        // The raw `pub const` set (kept for the FFI shim) must agree.
        assert_eq!(GZ_NONE, 0);
        assert_eq!(GZ_APPEND, 1);
        assert_eq!(GZ_READ, 7247);
        assert_eq!(GZ_WRITE, 31153);
    }

    #[test]
    fn how_sentinels_are_frozen() {
        assert_eq!(How::Look as i32, 0);
        assert_eq!(How::Copy as i32, 1);
        assert_eq!(How::Gzip as i32, 2);
        assert_eq!(LOOK, 0);
        assert_eq!(COPY, 1);
        assert_eq!(GZIP, 2);
    }

    #[test]
    fn gzbufsize_is_8192() {
        assert_eq!(GZBUFSIZE, 8192);
    }

    // --- enum conversions ---

    #[test]
    fn mode_as_i32_and_from_i32_round_trip() {
        for m in [Mode::None, Mode::Append, Mode::Read, Mode::Write] {
            assert_eq!(Mode::from_i32(m.as_i32()), Some(m));
        }
        // Anything outside the four sentinels is rejected.
        assert_eq!(Mode::from_i32(2), None);
        assert_eq!(Mode::from_i32(-1), None);
        assert_eq!(Mode::from_i32(7248), None);
    }

    #[test]
    fn how_as_i32_matches_discriminants() {
        assert_eq!(How::Look.as_i32(), 0);
        assert_eq!(How::Copy.as_i32(), 1);
        assert_eq!(How::Gzip.as_i32(), 2);
    }

    // --- constructor defaults (C `gz_open` + `gz_reset`) ---

    #[test]
    fn new_sets_gz_open_post_alloc_defaults() {
        let st = GzState::new(dummy_file(), "archive.gz".to_string(), Mode::Read);
        // shared open-time defaults
        assert_eq!(st.want, GZBUFSIZE);
        assert_eq!(st.size, 0, "buffers unallocated until first i/o");
        assert_eq!(st.level, Z_DEFAULT_COMPRESSION);
        assert_eq!(st.strategy, Z_DEFAULT_STRATEGY);
        assert_eq!(st.err, Z_OK);
        assert!(st.msg.is_none());
        assert_eq!(st.direct, 0);
        assert!(st.in_buf.is_empty());
        assert!(st.out_buf.is_empty());
        assert_eq!(st.mode, Mode::Read);
        assert!(st.file.is_some());
        assert_eq!(st.path, "archive.gz");
        // gz_reset-equivalent seeds
        assert_eq!(st.have, 0);
        assert_eq!(st.next, 0);
        assert_eq!(st.pos, 0);
        assert_eq!(st.junk, -1, "junk starts at -1 (first member marker)");
        assert_eq!(st.how, How::Look);
        assert!(!st.again);
        assert_eq!(st.start, 0);
        assert!(!st.eof);
        assert!(!st.past);
        assert!(!st.reset);
        assert_eq!(st.skip, 0);
        assert_eq!(st.in_next, 0);
        assert_eq!(st.in_avail, 0);
        assert!(st.finalize.is_none());
    }

    // --- accessors ---

    #[test]
    fn out_slice_reflects_next_and_have() {
        let mut st = GzState::new(dummy_file(), "f".to_string(), Mode::Read);
        st.out_buf = vec![10, 20, 30, 40, 50];
        st.next = 1;
        st.have = 3;
        assert_eq!(st.out_slice(), &[20, 30, 40]);
        st.have = 0;
        assert!(st.out_slice().is_empty());
    }

    #[test]
    fn reading_writing_predicates() {
        let r = GzState::new(dummy_file(), "f".to_string(), Mode::Read);
        assert!(r.is_reading());
        assert!(!r.is_writing());
        let w = GzState::new(dummy_file(), "f".to_string(), Mode::Write);
        assert!(w.is_writing());
        assert!(!w.is_reading());
    }

    #[test]
    fn gz_intmax_is_i32_max() {
        assert_eq!(GzState::gz_intmax(), i32::MAX as u32);
        assert_eq!(GzState::gz_intmax(), 2_147_483_647);
    }

    // --- gz_error / clear_error / error_message (C `gz_error`, `gzerror`) ---

    #[test]
    fn gz_error_formats_message_and_clears_have_on_fatal() {
        let mut st = GzState::new(dummy_file(), "myfile.gz".to_string(), Mode::Read);
        st.have = 99;
        st.gz_error(Z_DATA_ERROR, Some("invalid block"));
        assert_eq!(st.err, Z_DATA_ERROR);
        assert_eq!(st.msg.as_deref(), Some("myfile.gz: invalid block"));
        assert_eq!(st.have, 0, "a fatal error zeroes `have`");
        assert_eq!(st.error_message(), "myfile.gz: invalid block");
    }

    #[test]
    fn gz_error_buf_error_is_not_fatal() {
        let mut st = GzState::new(dummy_file(), "p".to_string(), Mode::Read);
        st.have = 42;
        st.gz_error(Z_BUF_ERROR, None);
        assert_eq!(st.err, Z_BUF_ERROR);
        assert_eq!(st.have, 42, "Z_BUF_ERROR is non-fatal; `have` untouched");
        assert!(st.msg.is_none());
        assert_eq!(st.error_message(), "");
    }

    #[test]
    fn gz_error_again_suppresses_have_reset() {
        let mut st = GzState::new(dummy_file(), "p".to_string(), Mode::Read);
        st.have = 7;
        st.again = true;
        // A normally-fatal code, but `again` (EAGAIN/EWOULDBLOCK) keeps `have`.
        st.gz_error(Z_DATA_ERROR, None);
        assert_eq!(st.err, Z_DATA_ERROR);
        assert_eq!(st.have, 7, "again=true leaves `have` intact for retry");
    }

    #[test]
    fn gz_error_mem_error_keeps_msg_none_but_reports_out_of_memory() {
        let mut st = GzState::new(dummy_file(), "p".to_string(), Mode::Write);
        st.gz_error(Z_MEM_ERROR, Some("ignored"));
        assert_eq!(st.err, Z_MEM_ERROR);
        assert!(st.msg.is_none(), "Z_MEM_ERROR never allocates a message");
        assert_eq!(st.error_message(), "out of memory");
    }

    #[test]
    fn gz_error_clears_previous_message() {
        let mut st = GzState::new(dummy_file(), "p".to_string(), Mode::Read);
        st.gz_error(Z_DATA_ERROR, Some("first"));
        assert!(st.msg.is_some());
        st.gz_error(Z_OK, None);
        assert_eq!(st.err, Z_OK);
        assert!(st.msg.is_none());
    }

    #[test]
    fn clear_error_resets_code_and_message() {
        let mut st = GzState::new(dummy_file(), "p".to_string(), Mode::Read);
        st.gz_error(Z_DATA_ERROR, Some("boom"));
        st.clear_error();
        assert_eq!(st.err, Z_OK);
        assert!(st.msg.is_none());
        assert_eq!(st.error_message(), "");
    }

    // --- Drop / finalize-hook behaviour (RAII, double-finalize guard) ---
    //
    // Each scenario uses its OWN `static` counter and finalize fn so the tests
    // remain correct under the parallel test runner.

    static FLUSH_WRITE: AtomicUsize = AtomicUsize::new(0);
    fn finalize_flush_write(_s: &mut GzState) -> i32 {
        FLUSH_WRITE.fetch_add(1, Ordering::SeqCst);
        Z_OK
    }

    #[test]
    fn drop_flushes_initialized_write_exactly_once() {
        let mut st = GzState::new(dummy_file(), "f".to_string(), Mode::Write);
        st.size = 1; // initialized
        st.direct = 0; // gzip (non-transparent)
        st.set_finalize(finalize_flush_write);
        assert_eq!(FLUSH_WRITE.load(Ordering::SeqCst), 0);
        drop(st);
        assert_eq!(
            FLUSH_WRITE.load(Ordering::SeqCst),
            1,
            "Drop runs the write-finalize hook exactly once"
        );
    }

    static FLUSH_UNINIT: AtomicUsize = AtomicUsize::new(0);
    fn finalize_uninit(_s: &mut GzState) -> i32 {
        FLUSH_UNINIT.fetch_add(1, Ordering::SeqCst);
        Z_OK
    }

    #[test]
    fn drop_skips_uninitialized_write() {
        let mut st = GzState::new(dummy_file(), "f".to_string(), Mode::Write);
        st.size = 0; // NOT initialized -> no flush
        st.set_finalize(finalize_uninit);
        drop(st);
        assert_eq!(
            FLUSH_UNINIT.load(Ordering::SeqCst),
            0,
            "no flush when size == 0 (buffers never allocated)"
        );
    }

    static FLUSH_READ: AtomicUsize = AtomicUsize::new(0);
    fn finalize_read(_s: &mut GzState) -> i32 {
        FLUSH_READ.fetch_add(1, Ordering::SeqCst);
        Z_OK
    }

    #[test]
    fn drop_does_not_flush_read_stream() {
        let mut st = GzState::new(dummy_file(), "f".to_string(), Mode::Read);
        st.size = 1;
        st.set_finalize(finalize_read); // even if present, reads must not flush
        drop(st);
        assert_eq!(FLUSH_READ.load(Ordering::SeqCst), 0);
    }

    static FLUSH_DOUBLE: AtomicUsize = AtomicUsize::new(0);
    fn finalize_double(_s: &mut GzState) -> i32 {
        FLUSH_DOUBLE.fetch_add(1, Ordering::SeqCst);
        Z_OK
    }

    #[test]
    fn explicit_close_prevents_double_finalize() {
        let mut st = GzState::new(dummy_file(), "f".to_string(), Mode::Write);
        st.size = 1;
        st.direct = 0;
        st.set_finalize(finalize_double);
        // Simulate the explicit close path (close.rs): take + run the hook,
        // observing the returned code.
        let code = if let Some(f) = st.take_finalize() {
            f(&mut st)
        } else {
            Z_OK
        };
        assert_eq!(code, Z_OK);
        assert_eq!(FLUSH_DOUBLE.load(Ordering::SeqCst), 1);
        st.mark_closed();
        assert_eq!(st.mode, Mode::None, "mark_closed neutralizes the handle");
        drop(st);
        assert_eq!(
            FLUSH_DOUBLE.load(Ordering::SeqCst),
            1,
            "Drop must not finalize a second time after an explicit close"
        );
    }

    static FLUSH_TRANSPARENT: AtomicUsize = AtomicUsize::new(0);
    fn finalize_transparent(_s: &mut GzState) -> i32 {
        FLUSH_TRANSPARENT.fetch_add(1, Ordering::SeqCst);
        Z_OK
    }

    #[test]
    fn drop_skips_transparent_write() {
        let mut st = GzState::new(dummy_file(), "f".to_string(), Mode::Write);
        st.size = 1;
        st.direct = 1; // transparent copy -> no deflate finalize on drop
        st.set_finalize(finalize_transparent);
        drop(st);
        assert_eq!(
            FLUSH_TRANSPARENT.load(Ordering::SeqCst),
            0,
            "transparent (direct != 0) writes are not deflate-finalized by Drop"
        );
    }
}
