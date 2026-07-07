//! Internal state for the gzip file-I/O layer (`gz*` API) of the `zlib-rs`
//! crate.
//!
//! This module is the foundational, self-contained core of the gz layer: it
//! defines [`GzState`] — the safe-Rust port of the C `gz_state` /
//! `gzFile_s` structures declared in `gzguts.h` — together with the open-mode
//! ([`GzMode`]) and read look-ahead ([`How`]) enums. Every other `src/gz/*.rs`
//! module (`open`, `read`, `write`, `close`) operates on the [`GzState`]
//! defined here, so this file is authored first and depends only on the crate's
//! lower layers ([`crate::stream`], [`crate::error`]) plus [`std`].
//!
//! # Relationship to the C `gz_state`
//!
//! The C library models an open gzip file as a heap-allocated `gz_state` whose
//! address is handed back to callers as the opaque `gzFile` pointer. Its first
//! member is an embedded `struct gzFile_s x` holding the three fields the fast
//! `gzgetc()` macro reads directly (`have`, `next`, `pos`); the remaining
//! members are private bookkeeping. This port reproduces **every** field, but
//! replaces the unsafe C idioms with safe Rust equivalents:
//!
//! | C construct                      | Rust translation                       |
//! |----------------------------------|----------------------------------------|
//! | `int fd`                         | owned [`File`] (RAII close)            |
//! | `unsigned char *in` / `*out`     | owned [`Vec<u8>`] buffers             |
//! | `unsigned char *next` (moving)   | [`usize`] index into `out_buf`        |
//! | `char *path` / `char *msg`       | [`String`] / [`Option<String>`]       |
//! | `z_stream strm` (in place)       | owned [`ZStream`] (in place)          |
//! | `free`/`inflateEnd`/`close(fd)`  | `Drop` (RAII)                          |
//!
//! Because the moving output cursor `x.next` is modelled as a `usize` **index**
//! into `out_buf` rather than a raw pointer, all buffer access in the sibling
//! modules is plain, bounds-checked slice indexing: the available output data is
//! the slice `out_buf[next .. next + have]`. This keeps the entire gz layer free
//! of `unsafe`. The `#[repr(C)]` `gzFile_s { have, next, pos }` layout that backs
//! the C `gzgetc` macro, and the opaque-pointer round-trip, are reconstructed
//! **only** at the FFI boundary in `src/ffi/gz.rs` — never here.
//!
//! # Feature gating and safety
//!
//! The gz layer is the one part of the crate that requires the standard library
//! (for file I/O), so the whole `gz` module tree is gated behind the `gz-io`
//! Cargo feature, which implies `std` + `gzip`. This module therefore freely
//! uses [`std::fs::File`] and the `std` prelude. It contains **zero `unsafe`**:
//! all raw-fd, raw-pointer, and C-string handling for the FFI `gz*` entry points
//! lives in `src/ffi/gz.rs`, not here.

use std::fs::File;

use crate::error::ReturnCode;
use crate::stream::ZStream;

/// Open mode of a gzip file, mirroring the integer `GZ_*` mode constants in
/// `gzguts.h`.
///
/// The discriminants are **load-bearing**: they match the C
/// `#define GZ_NONE 0`, `GZ_APPEND 1`, `GZ_READ 7247`, and `GZ_WRITE 31153`
/// verbatim. The two large, arbitrary-looking values (`7247`, `31153`) are the
/// exact integers zlib uses as a lightweight integrity check on the structure a
/// caller passes back through the `gz*` API, and they are mirrored by the FFI
/// layer, so they must never be altered.
///
/// # The transient [`GzMode::Append`] value
///
/// [`GzMode::Append`] never persists on an open file. It is produced only while
/// `open.rs` parses the mode string (the `'a'` flag). Immediately after a
/// successful append-open, `open.rs` seeks to end-of-file and overwrites the
/// mode with [`GzMode::Write`] — reproducing the C sequence
/// `LSEEK(fd, 0, SEEK_END); state->mode = GZ_WRITE;`. Consequently a live
/// [`GzState`] is only ever observed in [`GzMode::Read`] or [`GzMode::Write`]
/// (and briefly [`GzMode::None`] during construction/teardown); the public
/// `gzerror`/`gzclearerr` integrity check accepts exactly those two persistent
/// modes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(i32)]
pub enum GzMode {
    /// No mode set yet (`GZ_NONE = 0`): the structure is not a usable gzip file.
    None = 0,
    /// Append mode (`GZ_APPEND = 1`): a **transient** value replaced by
    /// [`GzMode::Write`] once the file has been opened and seeked to its end.
    Append = 1,
    /// Reading mode (`GZ_READ = 7247`): the file is open for decompression.
    Read = 7247,
    /// Writing mode (`GZ_WRITE = 31153`): the file is open for compression.
    Write = 31153,
}

/// The read look-ahead state, mirroring the `gz_state.how` values in `gzguts.h`
/// (`#define LOOK 0`, `COPY 1`, `GZIP 2`).
///
/// While reading, the layer must first inspect the leading bytes of the input to
/// decide whether it is looking at a gzip member, and then remember that
/// decision across calls. `How` records that decision. It is surfaced at the gz
/// module root at crate visibility (`pub(crate) use state::How;`, matching this
/// type's `pub(crate)` visibility) so the sibling read driver can name it
/// without reaching into this file directly.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum How {
    /// `LOOK = 0`: look at the input to decide whether it begins a gzip header.
    Look = 0,
    /// `COPY = 1`: copy input bytes straight through (the data is not gzip).
    Copy = 1,
    /// `GZIP = 2`: decompress the input as a gzip stream.
    Gzip = 2,
}

/// The complete internal state of an open gzip file — the safe-Rust port of the
/// C `gz_state` structure from `gzguts.h`.
///
/// A live `GzState` is owned through a `Box<GzState>` (the gz open functions
/// return `Result<Box<GzState>, _>`), which is the safe analogue of the C
/// opaque `gzFile` pointer. The struct is deliberately **not** `#[repr(C)]`:
/// the C-visible `#[repr(C)] gzFile_s { have, next, pos }` window that backs the
/// fast `gzgetc()` macro is reconstructed separately in `src/ffi/gz.rs`. All
/// fields are `pub(crate)` so the sibling `open`/`read`/`write`/`close` modules
/// can build and drive the state directly, while remaining opaque to external
/// callers.
///
/// The fields are grouped exactly as in the C definition: the exposed
/// `gzFile_s` window, then identity/configuration, the reading-only fields, the
/// writing-only fields, and finally the shared seek/error/stream state.
pub struct GzState {
    // -- exposed window (C `struct gzFile_s x`) ------------------------------
    /// Number of output bytes currently available at [`next`](Self::next)
    /// (C `x.have`).
    ///
    /// Together with [`next`](Self::next) this describes the not-yet-delivered
    /// output as the slice `out_buf[next .. next + have]`.
    pub(crate) have: usize,

    /// Offset into [`out_buf`](Self::out_buf) of the next byte to deliver
    /// (read path) or the next free byte (write path) — the safe index form of
    /// the C moving pointer `x.next`.
    ///
    /// Modelling the cursor as an index (rather than a raw pointer) is what
    /// keeps the gz layer free of `unsafe`: every access is bounds-checked slice
    /// indexing.
    pub(crate) next: usize,

    /// Current position in the uncompressed data stream (C `x.pos`,
    /// `z_off64_t`), i.e. the number of uncompressed bytes read or written so
    /// far. Kept as a signed [`i64`] to match the C 64-bit signed offset.
    pub(crate) pos: i64,

    // -- identity / configuration --------------------------------------------
    /// The open mode of the file (C `int mode`); see [`GzMode`].
    pub(crate) mode: GzMode,

    /// The owned OS file handle (replaces the C `int fd`).
    ///
    /// All I/O is performed through the safe [`std::io::Read`],
    /// [`std::io::Write`], and [`std::io::Seek`] traits on this handle; dropping
    /// the `GzState` closes the descriptor (the RAII replacement for the C
    /// `close(fd)`).
    pub(crate) file: File,

    /// The path (or synthetic `<fd:N>` name from `gzdopen`) used when building
    /// error messages (C `char *path`).
    pub(crate) path: String,

    /// The size of each allocated I/O buffer, or `0` when the buffers have not
    /// been allocated yet (C `unsigned size`).
    ///
    /// A value of `0` is the sentinel the read/write drivers test to lazily
    /// allocate [`in_buf`](Self::in_buf) / [`out_buf`](Self::out_buf) on first
    /// use, exactly as the C code does.
    pub(crate) size: usize,

    /// The caller-requested buffer size (C `unsigned want`), defaulting to
    /// `GZBUFSIZE` (8192) and adjustable via `gzbuffer`.
    ///
    /// This is the base size; the actual allocations are `want` for the
    /// non-doubled buffer and `want << 1` for the doubled one (see
    /// [`in_buf`](Self::in_buf) / [`out_buf`](Self::out_buf)).
    pub(crate) want: usize,

    /// The input buffer (C `unsigned char *in`).
    ///
    /// On the **write** path this is sized `want << 1` (double) so `gzprintf`
    /// has room to format before compressing.
    pub(crate) in_buf: Vec<u8>,

    /// The output buffer (C `unsigned char *out`).
    ///
    /// On the **read** path this is sized `want << 1` (double) to guarantee room
    /// for `gzungetc` push-back and for the raw pass-through copy. The
    /// [`next`](Self::next)/[`have`](Self::have) window indexes into this
    /// buffer.
    pub(crate) out_buf: Vec<u8>,

    /// Tri-state transparency flag (C `int direct`); the `-1`/`0`/`1` values are
    /// preserved exactly, so this is kept as an [`i32`] rather than a `bool`.
    ///
    /// While **reading**: `1` = transparent/auto-detect (copy input directly),
    /// `-1` = force gzip only (the `'G'` open flag), `0` = currently processing
    /// a gzip member. While **writing**: `0` = gzip framing, `1` = transparent
    /// pass-through (the `'T'` open flag).
    pub(crate) direct: i32,

    // -- reading only --------------------------------------------------------
    /// The read look-ahead state (C `int how`); see [`How`].
    pub(crate) how: How,

    /// Trailing-junk classification while reading (C `int junk`): `-1` = at the
    /// start, `1` = a candidate for trailing junk after a gzip member, `0` =
    /// confirmed inside a real gzip stream.
    pub(crate) junk: i32,

    /// `true` if the last I/O returned `EAGAIN`/`EWOULDBLOCK` on a non-blocking
    /// descriptor (C `int again`).
    ///
    /// When set, [`error`](Self::error) does **not** clear
    /// [`have`](Self::have), so a retry can still make progress.
    pub(crate) again: bool,

    /// Offset into [`in_buf`](Self::in_buf) of the next unconsumed *compressed*
    /// input byte — the safe index form of the C `z_stream.next_in` pointer as
    /// used by the read layer.
    ///
    /// In reference zlib the input-buffer cursor lives on the embedded
    /// `z_stream` (`strm.next_in` / `strm.avail_in`). This crate's idiomatic
    /// [`ZStream`] deliberately carries **no** `next_in`/`avail_in` fields —
    /// input is handed to the engine as a slice on every call and progress is
    /// reported back explicitly — so the read driver (`read.rs`) must itself
    /// remember how much of [`in_buf`](Self::in_buf) is still unconsumed between
    /// calls (compressed input frequently survives a call when the output buffer
    /// fills before the input is exhausted). Together with
    /// [`in_avail`](Self::in_avail) this describes the not-yet-decompressed input
    /// as the slice `in_buf[in_next .. in_next + in_avail]`.
    pub(crate) in_next: usize,

    /// Number of unconsumed *compressed* input bytes available at
    /// [`in_next`](Self::in_next) (C `z_stream.avail_in`, relocated onto the gz
    /// state — see [`in_next`](Self::in_next) for why).
    pub(crate) in_avail: usize,

    /// The file position where the gzip data started, used as the rewind anchor
    /// for `gzrewind`/`gzseek` (C `z_off64_t start`).
    pub(crate) start: i64,

    /// `true` once the end of the input file has been reached (C `int eof`).
    pub(crate) eof: bool,

    /// `true` if a read requested data past the end of the file (C `int past`);
    /// this is precisely the condition `gzeof` reports.
    pub(crate) past: bool,

    // -- writing only --------------------------------------------------------
    /// The compression level in effect for writing (C `int level`).
    pub(crate) level: i32,

    /// The compression strategy in effect for writing (C `int strategy`).
    pub(crate) strategy: i32,

    /// `true` if a `deflateReset` is pending after a `Z_FINISH` (C `int reset`),
    /// so the next write reinitialises the deflate stream for a new member.
    pub(crate) reset: bool,

    // -- shared --------------------------------------------------------------
    /// The pending seek amount, in bytes (C `z_off64_t skip`): data to skip on
    /// the next read, or zeros to write on the next write. Already rewound if
    /// the seek was backwards.
    pub(crate) skip: i64,

    /// The last error code recorded on this file (C `int err`); see
    /// [`ReturnCode`]. Reported to callers by `gzerror`.
    pub(crate) err: ReturnCode,

    /// The last error message (C `char *msg`), or [`None`] when there is no
    /// message.
    ///
    /// Populated by [`error`](Self::error) as `"{path}: {message}"`. For
    /// [`ReturnCode::MemError`] this is deliberately left [`None`]: the public
    /// `gzerror` synthesises the literal `"out of memory"` instead of storing a
    /// heap string, matching the C behaviour of not allocating while out of
    /// memory.
    pub(crate) msg: Option<String>,

    /// The embedded (de)compression stream (C `z_stream strm`, stored in place —
    /// not a pointer).
    ///
    /// [`GzState`] owns this [`ZStream`] directly; the read/write drivers drive
    /// (de)compression by calling the crate's `inflate`/`deflate` entry points
    /// on `&mut self.strm`. When the `GzState` is dropped, the `ZStream`'s own
    /// `Drop` performs the `inflateEnd`/`deflateEnd`-equivalent teardown.
    pub(crate) strm: ZStream,
}

impl Drop for GzState {
    /// Releases every resource the `GzState` owns.
    ///
    /// The body is intentionally empty because release is fully handled by RAII:
    ///
    /// * [`in_buf`](GzState::in_buf) / [`out_buf`](GzState::out_buf) (`Vec<u8>`)
    ///   free their backing storage — subsuming the C `free(state->in)` /
    ///   `free(state->out)`;
    /// * [`strm`](GzState::strm) (`ZStream`) runs its own `Drop`, which performs
    ///   the `inflateEnd`/`deflateEnd`-equivalent teardown of the engine state;
    ///   and
    /// * [`file`](GzState::file) (`File`) closes the underlying descriptor —
    ///   subsuming the C `close(fd)`.
    ///
    /// This explicit `Drop` therefore exists to *document* the RAII contract
    /// (and to give the sibling modules a single place to reason about
    /// teardown); the compiler-generated field drops would achieve the same
    /// release.
    ///
    /// **Flushing is deliberately not performed here.** For the write path the
    /// final `deflate(..., Z_FINISH)` flush — and, crucially, the reporting of
    /// any I/O error it encounters — is done explicitly by `gzclose_w` in
    /// `close.rs`. This matches zlib's contract, where an explicit `gzclose*`
    /// call is required to finalise output; flushing from `Drop` would silently
    /// swallow write errors that the caller must be able to observe.
    fn drop(&mut self) {
        // No manual work: `Vec`, `ZStream`, and `File` each release themselves.
    }
}

impl GzState {
    /// Records an error on this file — the port of the internal C
    /// `gz_error(gz_statep, int, const char *)` from `gzlib.c`.
    ///
    /// This is implemented as a method on the state (rather than a free function
    /// in `open.rs`) because it only ever touches this state's `err`, `msg`, and
    /// `have` fields. Placing it here lets `read.rs`, `write.rs`, and `open.rs`
    /// all call `self.error(...)` without depending on one another, which
    /// deliberately breaks what would otherwise be a circular module
    /// dependency.
    ///
    /// The behaviour mirrors the C function exactly:
    ///
    /// 1. Any previous message is dropped (`Vec`/`String` frees it — the safe
    ///    analogue of the C `free(state->msg)`).
    /// 2. If the error is **fatal** — not [`ReturnCode::Ok`], not
    ///    [`ReturnCode::BufError`], and not while a non-blocking retry is
    ///    pending ([`again`](Self::again)) — then [`have`](Self::have) is zeroed
    ///    so the fast `gzgetc()` path fails and control funnels back through the
    ///    slow path that consults `err`.
    /// 3. The error code is recorded in [`err`](Self::err).
    /// 4. With no message, there is nothing more to do.
    /// 5. For [`ReturnCode::MemError`] no message is stored (allocating while
    ///    out of memory is exactly what must be avoided); the public `gzerror`
    ///    returns the literal `"out of memory"` for that code.
    /// 6. Otherwise the stored message is `"{path}: {message}"`, matching the C
    ///    `snprintf(..., "%s%s%s", path, ": ", msg)`.
    ///
    /// # Parameters
    ///
    /// * `err` — the [`ReturnCode`] to record.
    /// * `msg` — the human-readable detail to attach, or [`None`] to record only
    ///   the code (as the C code does when passed a `NULL` message).
    pub(crate) fn error(&mut self, err: ReturnCode, msg: Option<&str>) {
        // 1. Drop any previously stored message (Rust frees the old `String`).
        self.msg = None;

        // 2. If the error is fatal and we are not mid non-blocking retry, zero
        //    `have` so that the fast `gzgetc()` macro path fails and defers to
        //    the error-aware slow path.
        if err != ReturnCode::Ok && err != ReturnCode::BufError && !self.again {
            self.have = 0;
        }

        // 3. Record the error code.
        self.err = err;

        // 4. No message supplied — done.
        let Some(msg) = msg else {
            return;
        };

        // 5. For an out-of-memory error, do not allocate: leave `msg` as `None`
        //    (the public `gzerror` reports the static "out of memory").
        if err == ReturnCode::MemError {
            return;
        }

        // 6. Construct the "path: message" detail string.
        self.msg = Some(format!("{}: {msg}", self.path));
    }

    /// Clears any recorded error, returning the file to the
    /// [`ReturnCode::Ok`] / no-message state.
    ///
    /// This is the state-mutating half of the C `gzclearerr`, which calls
    /// `gz_error(state, Z_OK, NULL)`; the caller (`gzclearerr` in `open.rs`) is
    /// responsible for also clearing [`eof`](Self::eof)/[`past`](Self::past) on
    /// the read path. It is likewise used to reset the error state at the start
    /// of the `gz*` entry points.
    pub(crate) fn clear_error(&mut self) {
        self.error(ReturnCode::Ok, None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a `GzState` suitable for exercising the state-only methods.
    ///
    /// [`GzState::error`] / [`GzState::clear_error`] touch only `err`, `msg`,
    /// `have`, `again`, and `path`, so the remaining fields are filled with
    /// innocuous defaults. A real, always-openable file — the running test
    /// binary, falling back to `/dev/null` — satisfies the [`File`] field with
    /// **no `unsafe`** and no external test dependency; no I/O is performed on
    /// it here.
    fn test_state(path: &str) -> GzState {
        let file = std::env::current_exe()
            .ok()
            .and_then(|p| File::open(p).ok())
            .or_else(|| File::open("/dev/null").ok())
            .expect("a real, openable file is required for the test GzState");
        GzState {
            have: 0,
            next: 0,
            pos: 0,
            mode: GzMode::Read,
            file,
            path: String::from(path),
            size: 0,
            want: 0,
            in_buf: Vec::new(),
            out_buf: Vec::new(),
            direct: 0,
            how: How::Look,
            junk: -1,
            again: false,
            in_next: 0,
            in_avail: 0,
            start: 0,
            eof: false,
            past: false,
            level: 0,
            strategy: 0,
            reset: false,
            skip: 0,
            err: ReturnCode::Ok,
            msg: None,
            strm: ZStream::new(),
        }
    }

    #[test]
    fn gz_mode_discriminants_match_gzguts_h() {
        // Exact `#define`s from gzguts.h — mirrored by the FFI layer.
        assert_eq!(GzMode::None as i32, 0);
        assert_eq!(GzMode::Append as i32, 1);
        assert_eq!(GzMode::Read as i32, 7247);
        assert_eq!(GzMode::Write as i32, 31153);
    }

    #[test]
    fn how_discriminants_match_gzguts_h() {
        // Exact LOOK / COPY / GZIP values from gzguts.h.
        assert_eq!(How::Look as u8, 0);
        assert_eq!(How::Copy as u8, 1);
        assert_eq!(How::Gzip as u8, 2);
    }

    #[test]
    fn error_fatal_clears_have_and_sets_message() {
        let mut s = test_state("archive.gz");
        s.have = 42;
        s.error(ReturnCode::DataError, Some("bad data"));
        assert_eq!(s.err, ReturnCode::DataError);
        assert_eq!(s.have, 0, "a fatal error must clear `have`");
        assert_eq!(s.msg.as_deref(), Some("archive.gz: bad data"));
    }

    #[test]
    fn error_ok_preserves_have_and_drops_previous_message() {
        let mut s = test_state("archive.gz");
        s.have = 42;
        s.msg = Some(String::from("stale message"));
        s.error(ReturnCode::Ok, None);
        assert_eq!(s.err, ReturnCode::Ok);
        assert_eq!(s.have, 42, "Z_OK is not fatal, so `have` is preserved");
        assert_eq!(s.msg, None, "the previously stored message must be dropped");
    }

    #[test]
    fn error_buf_error_preserves_have() {
        let mut s = test_state("archive.gz");
        s.have = 42;
        s.error(ReturnCode::BufError, None);
        assert_eq!(s.err, ReturnCode::BufError);
        assert_eq!(
            s.have, 42,
            "Z_BUF_ERROR is explicitly treated as non-fatal for `have`"
        );
        assert_eq!(s.msg, None);
    }

    #[test]
    fn error_again_preserves_have_even_when_fatal() {
        let mut s = test_state("archive.gz");
        s.have = 42;
        s.again = true;
        s.error(ReturnCode::DataError, Some("would block"));
        assert_eq!(s.err, ReturnCode::DataError);
        assert_eq!(
            s.have, 42,
            "a pending non-blocking retry (`again`) must preserve `have`"
        );
        assert_eq!(s.msg.as_deref(), Some("archive.gz: would block"));
    }

    #[test]
    fn error_mem_error_stores_no_message() {
        let mut s = test_state("archive.gz");
        s.have = 42;
        s.error(ReturnCode::MemError, Some("ignored detail"));
        assert_eq!(s.err, ReturnCode::MemError);
        // MemError is fatal (not Ok / not BufError), so `have` is still cleared.
        assert_eq!(s.have, 0);
        // ...but no message is allocated: the public `gzerror` synthesises the
        // literal "out of memory" for this code.
        assert_eq!(s.msg, None);
    }

    #[test]
    fn error_message_uses_path_prefix() {
        let mut s = test_state("/tmp/data.gz");
        s.error(ReturnCode::StreamError, Some("boom"));
        // Format is exactly "{path}: {msg}" (C snprintf "%s%s%s", path, ": ", msg).
        assert_eq!(s.msg.as_deref(), Some("/tmp/data.gz: boom"));
    }

    #[test]
    fn clear_error_resets_code_and_message() {
        let mut s = test_state("archive.gz");
        s.error(ReturnCode::DataError, Some("bad"));
        assert_eq!(s.err, ReturnCode::DataError);
        assert!(s.msg.is_some());

        s.clear_error();
        assert_eq!(s.err, ReturnCode::Ok);
        assert_eq!(s.msg, None);
    }
}
