//! Return codes and the human-readable status messages that accompany them.
//!
//! This is the foundational module of the crate: every fallible operation in
//! `zlib-rs` reports its outcome as a [`ReturnCode`].
//!
//! The ten-element table defined here is the provenance of the **generic,
//! per-code** messages -- the ones `zError` returns, one short phrase for each
//! status value. It is deliberately **not** the provenance of every message a C
//! caller can observe, and assuming otherwise would misread both this module and
//! the reference implementation. Measured against the C sources:
//!
//! * `zError` (`zutil.c` L139-L141) is nothing but `ERR_MSG`, so it is served
//!   entirely from this table.
//! * `deflate.c` sets `strm->msg` from the table exactly once, at L511
//!   (`ERR_MSG(Z_MEM_ERROR)`); otherwise it only clears it to `Z_NULL`.
//! * The **inflate state machine constructs its own, far more specific
//!   messages**: `inflate.c` performs 21 literal `strm->msg` assignments drawing
//!   on 18 distinct strings -- "incorrect header check", "invalid stored block
//!   lengths", "invalid distance too far back", "incorrect data check" and the
//!   rest. None of them appears in `z_errmsg`, and a caller reading `strm->msg`
//!   after a failed `inflate()` normally sees one of those, not "data error".
//! * The **gzip layer constructs its own too**: `gz_error` (`gzlib.c`
//!   L555-L588) allocates and formats `"<path>: <reason>"`, which is what
//!   `gzerror` reports. That string is per-stream heap data, not a table entry.
//!
//! So: this table backs `zError` and the single `deflate.c` memory-error path.
//! The specific diagnostics live with the code that detects the condition, which
//! is where the reference implementation puts them and where this port keeps
//! them.
//!
//! # Correspondence with the reference implementation
//!
//! * [`ReturnCode`]'s associated constants are the nine codes defined in
//!   `zlib.h` L181-L189. As the note at `zlib.h` L190-L192 records, negative
//!   values are errors while positive values report "special but normal events";
//!   [`ReturnCode::is_error`] encodes exactly that rule.
//! * The crate-private `Z_ERRMSG` table reproduces `z_errmsg[10]` from
//!   `zutil.c` L13-L24 character for character, including its ten-slot shape.
//! * [`err_msg`] reproduces the `ERR_MSG` macro from `zutil.h` L65, which is the
//!   entire body of `zError` (`zutil.c` L139-L141).
//! * The crate-private `err_return` helper reproduces the `ERR_RETURN` macro from
//!   `zutil.h` L67-L68, and [`ReturnCode::record_msg`] is its method form.
//!
//! # Layering and safety posture
//!
//! The module is `no_std`, allocation-free and dependency-free: it names only
//! `core`. It holds no raw pointer, declares neither a C-visible layout nor
//! C-visible linkage for any item, and needs no escape hatch from the compiler's
//! memory-safety guarantees -- so the crate root's blanket prohibition on such
//! escape hatches costs this module nothing. Turning these `&'static str`
//! messages into NUL-terminated C strings, and exporting the `zError` symbol
//! itself, are the business of the `libz-rs-sys` facade crate, which is the only
//! crate in the workspace permitted to **use** `unsafe` -- and therefore the only
//! one that can dereference a pointer or perform an FFI call. That is a narrower
//! and more accurate statement than "the only crate that holds a raw pointer":
//! this crate does hold two raw pointer values, `allocate::Opaque` and
//! `gz::state::GzFileExposed::next`, neither of which it can ever read through.
//! Neither appears in this module.
//!
//! The version script `zlib.map` lists `z_errmsg` in its `local:` block, so the
//! raw table is deliberately crate-private and only the accessors are public.
//!
//! # Examples
//!
//! ```
//! use zlib_rs::{err_msg, ReturnCode};
//!
//! // Status codes convert losslessly to and from the C `int` representation.
//! let code = ReturnCode::BUF_ERROR;
//! assert_eq!(code.as_i32(), -5);
//! assert_eq!(ReturnCode::from_i32(-5), Some(code));
//! assert_eq!(code.msg(), "buffer error");
//!
//! // An undocumented code is not silently invented ...
//! assert_eq!(ReturnCode::from_i32(42), None);
//! // ... but `err_msg` still answers for any `int`, exactly as `zError` does.
//! assert_eq!(err_msg(42), "");
//! ```
//!
//! # Provenance
//!
//! Return codes and the messages that accompany them.
//!
//! Ported from the `Z_*` constants (`zlib.h` L181-L189) and the `z_errmsg[10]` table
//! (`zutil.c` L13-L24). The raw table stays crate-private because `zlib.map` hides `z_errmsg`;
//! only the accessors are public.

// These private constants are the single source of truth for the numeric values.
// They are named exactly as `zlib.h` spells them so that a reviewer diffing this
// module against the header can match them line for line, and they are reused
// three ways: to define the `ReturnCode` constants, as the match arms of
// `ReturnCode::from_i32`, and as the bounds of the range pattern in
// `err_msg_index`.

/// `Z_OK` (0) -- the operation completed successfully.
///
/// Mirrors `zlib.h` L181.
const Z_OK: i32 = 0;

/// `Z_STREAM_END` (1) -- the end of the compressed stream was reached.
///
/// Mirrors `zlib.h` L182.
const Z_STREAM_END: i32 = 1;

/// `Z_NEED_DICT` (2) -- a preset dictionary is required before decompression can
/// continue.
///
/// Mirrors `zlib.h` L183.
const Z_NEED_DICT: i32 = 2;

/// `Z_ERRNO` (-1) -- an underlying file operation failed and the platform's
/// `errno` holds the detail.
///
/// Mirrors `zlib.h` L184.
const Z_ERRNO: i32 = -1;

/// `Z_STREAM_ERROR` (-2) -- the stream state is inconsistent, or a parameter was
/// invalid.
///
/// Mirrors `zlib.h` L185.
const Z_STREAM_ERROR: i32 = -2;

/// `Z_DATA_ERROR` (-3) -- the input data is corrupt, or is not in the format the
/// stream was configured to read.
///
/// Mirrors `zlib.h` L186.
const Z_DATA_ERROR: i32 = -3;

/// `Z_MEM_ERROR` (-4) -- an allocation failed.
///
/// Mirrors `zlib.h` L187.
const Z_MEM_ERROR: i32 = -4;

/// `Z_BUF_ERROR` (-5) -- no progress was possible; more input, or more output
/// room, is required.
///
/// Mirrors `zlib.h` L188.
const Z_BUF_ERROR: i32 = -5;

/// `Z_VERSION_ERROR` (-6) -- the caller was compiled against an incompatible
/// version of the library.
///
/// Mirrors `zlib.h` L189.
const Z_VERSION_ERROR: i32 = -6;

/// Index of the trailing sentinel slot of `Z_ERRMSG`, which every code outside
/// `Z_VERSION_ERROR ..= Z_NEED_DICT` maps to.
///
/// Mirrors the literal `9` in the `ERR_MSG` macro, `zutil.h` L65.
const OUT_OF_RANGE_INDEX: usize = 9;

/// The status-message table, indexed by `2 - code`.
///
/// Transcribed character for character from `z_errmsg[10]` in `zutil.c`
/// L13-L24, in the same order, and declared with the same explicit length.
/// `zutil.h` L62-L63 documents both properties: the array is "indexed by
/// 2-zlib_error", and the size is given explicitly "to avoid silly warnings with
/// Visual C++". The two empty strings are therefore kept as distinct entries --
/// slot 2 belongs to `Z_OK`, slot 9 is the out-of-range sentinel -- and must not
/// be collapsed into one.
///
/// The entries are `&str` rather than NUL-terminated bytes because C-string
/// construction belongs to the `libz-rs-sys` facade: the safe core never holds a
/// pointer, so it has no use for the terminator, and keeping the strings as
/// ordinary Rust string slices lets the core compare and log them directly.
///
/// The table is crate-private on purpose. `zlib.map` lists `z_errmsg` in the
/// `local:` block of its `ZLIB_1.2.0` node, so the symbol must never be
/// exported; keeping the Rust item `pub(crate)` and unexported satisfies that by
/// construction, and callers outside the crate use [`err_msg`] instead.
///
/// Mirrors `zutil.c` L13-L24 (`z_errmsg`).
pub(crate) const Z_ERRMSG: [&str; 10] = [
    "need dictionary",      // Z_NEED_DICT       2
    "stream end",           // Z_STREAM_END      1
    "",                     // Z_OK              0
    "file error",           // Z_ERRNO         (-1)
    "stream error",         // Z_STREAM_ERROR  (-2)
    "data error",           // Z_DATA_ERROR    (-3)
    "insufficient memory",  // Z_MEM_ERROR     (-4)
    "buffer error",         // Z_BUF_ERROR     (-5)
    "incompatible version", // Z_VERSION_ERROR (-6)
    "",                     // out-of-range sentinel
];

/// A zlib status code: the value every entry point of the engine returns.
///
/// This is a newtype over the C `int` rather than an enumeration of bare
/// integers, so that a status can never be confused with, or accidentally
/// arithmetically combined with, a byte count or a length. No arithmetic
/// operators are implemented for that reason, and there is deliberately no
/// `Default`: no status is a meaningful "zero value" for an uninitialised
/// operation. The wrapped integer is private, and the only way to build a value
/// is from one of the nine associated constants or through
/// [`from_i32`](ReturnCode::from_i32) -- which rejects anything undocumented,
/// so a bogus code can never be laundered through this type and handed back to a
/// C caller.
///
/// The full set of codes, with the exact `int` each one carries:
///
/// | Constant | Value | Message |
/// |---|---|---|
/// | [`OK`](ReturnCode::OK) | 0 | `""` |
/// | [`STREAM_END`](ReturnCode::STREAM_END) | 1 | `"stream end"` |
/// | [`NEED_DICT`](ReturnCode::NEED_DICT) | 2 | `"need dictionary"` |
/// | [`ERRNO`](ReturnCode::ERRNO) | -1 | `"file error"` |
/// | [`STREAM_ERROR`](ReturnCode::STREAM_ERROR) | -2 | `"stream error"` |
/// | [`DATA_ERROR`](ReturnCode::DATA_ERROR) | -3 | `"data error"` |
/// | [`MEM_ERROR`](ReturnCode::MEM_ERROR) | -4 | `"insufficient memory"` |
/// | [`BUF_ERROR`](ReturnCode::BUF_ERROR) | -5 | `"buffer error"` |
/// | [`VERSION_ERROR`](ReturnCode::VERSION_ERROR) | -6 | `"incompatible version"` |
///
/// Mirrors `zlib.h` L181-L192 (the `Z_OK` .. `Z_VERSION_ERROR` return-code
/// block and its explanatory comment).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ReturnCode(i32);

impl ReturnCode {
    /// No error: the operation did exactly what was asked of it.
    ///
    /// Mirrors `Z_OK`, `zlib.h` L181.
    pub const OK: Self = Self(Z_OK);

    /// The end of the compressed stream was reached. Not an error: a caller that
    /// has finished a stream sees this rather than [`OK`](Self::OK).
    ///
    /// Mirrors `Z_STREAM_END`, `zlib.h` L182.
    pub const STREAM_END: Self = Self(Z_STREAM_END);

    /// Decompression needs the preset dictionary that the stream was compressed
    /// with; supply it and resume. Not an error, which is why the value is
    /// positive.
    ///
    /// Mirrors `Z_NEED_DICT`, `zlib.h` L183.
    pub const NEED_DICT: Self = Self(Z_NEED_DICT);

    /// An underlying file operation failed; the platform's `errno` holds the
    /// detail.
    ///
    /// This code is produced **only** by the `gz*` file layer, which is the only
    /// part of the library that touches the file system. The deflate and inflate
    /// engines never return it, because they operate purely on caller-supplied
    /// buffers and have no file descriptor to fail on.
    ///
    /// Mirrors `Z_ERRNO`, `zlib.h` L184.
    pub const ERRNO: Self = Self(Z_ERRNO);

    /// The stream state is inconsistent, or an argument was invalid -- for
    /// instance a null stream, a stream that was never initialised, or a
    /// parameter outside its documented range.
    ///
    /// Mirrors `Z_STREAM_ERROR`, `zlib.h` L185.
    pub const STREAM_ERROR: Self = Self(Z_STREAM_ERROR);

    /// The input is corrupt, or is not in the format this stream was configured
    /// to read: a bad header check, an invalid code, a failed checksum.
    ///
    /// Mirrors `Z_DATA_ERROR`, `zlib.h` L186.
    pub const DATA_ERROR: Self = Self(Z_DATA_ERROR);

    /// An allocation failed.
    ///
    /// Which allocator failed depends on how the stream was created, and there are
    /// two paths, mirroring the reference implementation's own two:
    ///
    /// * **Caller hooks.** When a `z_stream` arrives with non-null `zalloc`/`zfree`,
    ///   the facade injects an allocator that calls them, so this status reports the
    ///   *caller's* allocator declining a request. `test/infcover.c` drives exactly
    ///   this path with its tracking allocator and `mem_limit` (L176-L181).
    /// * **The default path.** When `zalloc`/`zfree` are `Z_NULL`, C substitutes
    ///   `zcalloc`/`zcfree`, which are `malloc`/`free` (`zutil.c` L301). The port's
    ///   counterpart is [`crate::allocate::GlobalAllocator`], which allocates from
    ///   **Rust's global allocator**; the one-shot `compress`/`uncompress` wrappers,
    ///   which have no stream to take hooks from, always use it. In that
    ///   configuration this status does report a Rust global-allocation failure.
    ///
    /// Either way the failure is *reported*, never fatal: [`crate::allocate::Buffer`]
    /// goes through `try_reserve_exact` rather than an infallible constructor, which
    /// is what keeps this status reachable at all.
    ///
    /// Mirrors `Z_MEM_ERROR`, `zlib.h` L187.
    pub const MEM_ERROR: Self = Self(Z_MEM_ERROR);

    /// No progress was possible on this call: the operation needs more input, or
    /// more room in the output buffer, before it can do anything. Recoverable --
    /// provide more of either and call again.
    ///
    /// Mirrors `Z_BUF_ERROR`, `zlib.h` L188.
    pub const BUF_ERROR: Self = Self(Z_BUF_ERROR);

    /// The caller was compiled against an incompatible version of the library.
    ///
    /// The comparison is narrower than "the version strings disagree", and the
    /// facade must reproduce it exactly. `deflateInit_` (`deflate.c` L392-L397),
    /// `inflateInit2_` (`inflate.c` L178-L180) and `inflateBackInit_`
    /// (`infback.c` L30-L32) return this status when any of three conditions
    /// holds:
    ///
    /// 1. the caller passed a null `version` pointer;
    /// 2. `version[0] != ZLIB_VERSION[0]` -- **the first character only**, making
    ///    it a major-version check rather than a full-string comparison, so a
    ///    caller built against `"1.2.11"` is accepted and one built against
    ///    `"2.0.0"` is not;
    /// 3. the caller's `stream_size` differs from `sizeof(z_stream)`, which
    ///    catches a caller compiled against a different `z_stream` layout.
    ///
    /// All three are evaluated *before* the stream pointer is checked, so a null
    /// stream passed with a bad version yields this status and not
    /// [`ReturnCode::STREAM_ERROR`].
    ///
    /// Mirrors `Z_VERSION_ERROR`, `zlib.h` L189.
    pub const VERSION_ERROR: Self = Self(Z_VERSION_ERROR);

    /// Returns the exact C `int` this status carries.
    ///
    /// This is the value that reaches a C caller verbatim, so it is fixed by the
    /// ABI: 0, 1, 2, -1, -2, -3, -4, -5 and -6 respectively.
    ///
    /// Mirrors `zlib.h` L181-L189.
    #[must_use]
    pub const fn as_i32(self) -> i32 {
        self.0
    }

    /// Converts a raw C `int` into a status, returning [`None`] for any value
    /// that is not one of the nine documented codes.
    ///
    /// The conversion is total: every `i32` -- including [`i32::MIN`] and
    /// [`i32::MAX`] -- maps to either a documented code or [`None`], and no
    /// input can make this function panic. It is deliberately fallible rather
    /// than lenient: admitting an unknown integer would let a value that no
    /// caller can interpret travel back out across the C boundary, so undocumented
    /// codes are rejected here instead. Use [`err_msg`] when a message is wanted
    /// for an arbitrary integer, as `zError` allows.
    ///
    /// Mirrors `zlib.h` L181-L189.
    #[must_use]
    pub const fn from_i32(code: i32) -> Option<Self> {
        match code {
            Z_OK => Some(Self::OK),
            Z_STREAM_END => Some(Self::STREAM_END),
            Z_NEED_DICT => Some(Self::NEED_DICT),
            Z_ERRNO => Some(Self::ERRNO),
            Z_STREAM_ERROR => Some(Self::STREAM_ERROR),
            Z_DATA_ERROR => Some(Self::DATA_ERROR),
            Z_MEM_ERROR => Some(Self::MEM_ERROR),
            Z_BUF_ERROR => Some(Self::BUF_ERROR),
            Z_VERSION_ERROR => Some(Self::VERSION_ERROR),
            _ => None,
        }
    }

    /// Returns the human-readable message paired with this status.
    ///
    /// The convenience form of [`err_msg`] for a value that is already known to
    /// be a documented code. [`OK`](Self::OK) maps to the empty string, exactly
    /// as slot 2 of the C table does.
    ///
    /// Mirrors `zutil.h` L65 (`ERR_MSG`) and `zutil.c` L13-L24
    /// (`z_errmsg`).
    #[must_use]
    pub fn msg(self) -> &'static str {
        err_msg(self.0)
    }

    /// Returns `true` when this status reports a genuine error.
    ///
    /// The rule is the one stated at `zlib.h` L190-L192: negative values are
    /// errors, and positive values report "special but normal events". So
    /// [`STREAM_END`](Self::STREAM_END) and [`NEED_DICT`](Self::NEED_DICT) are
    /// **not** errors, and neither is [`OK`](Self::OK).
    ///
    /// Mirrors `zlib.h` L190-L192.
    #[must_use]
    pub const fn is_error(self) -> bool {
        self.0 < Z_OK
    }

    /// Records this status's message in `msg_slot` and yields the status back, so
    /// that reporting a failure stays a single expression.
    ///
    /// The method form of the crate-private `err_return` helper, and therefore of
    /// the `ERR_RETURN` macro at `zutil.h` L67-L68. It exists as a public method
    /// because `err_return` is `pub(crate)` and so cannot be reached from the
    /// `libz-rs-sys` facade, which lives in a separate crate and needs the same
    /// operation when it populates the `msg` field of a `z_stream`.
    ///
    /// The slot is an [`Option`] because C's `msg` field is a pointer that is
    /// genuinely null for much of a stream's life -- `inflate.c` L106 and L182
    /// both assign `Z_NULL` to it -- and [`None`] is the faithful Rust spelling
    /// of that state. This method always stores [`Some`], never [`None`]: like
    /// `ERR_MSG`, it yields the empty string rather than nothing when a code has
    /// no text.
    ///
    /// As `zutil.h` L69 warns of the macro, this is to be used only when the
    /// stream state is already known to be valid; a caller that has not yet
    /// validated the state has no slot it may legally write to.
    ///
    /// Mirrors `zutil.h` L67-L68 (`ERR_RETURN`).
    #[must_use]
    pub fn record_msg(self, msg_slot: &mut Option<&'static str>) -> Self {
        err_return(msg_slot, self)
    }
}

/// Converts a status into the C `int` it carries.
///
/// The idiomatic counterpart of [`ReturnCode::as_i32`], which is the `const`
/// form and the one the engine itself uses. Both are lossless and total.
///
/// Mirrors `zlib.h` L181-L189.
impl From<ReturnCode> for i32 {
    fn from(code: ReturnCode) -> Self {
        code.as_i32()
    }
}

/// Returns the status message for an arbitrary C `int`, mapping any value
/// outside the documented range to the empty string.
///
/// This is the Rust equivalent of the `ERR_MSG` macro at `zutil.h` L65, and
/// hence of `zError` itself, whose entire body is `return ERR_MSG(err)`
/// (`zutil.c` L139-L141). The parameter is a raw `i32` rather than a
/// [`ReturnCode`] precisely because `zError` accepts any `int`: narrowing the
/// signature would reject calls that the C API has always permitted.
///
/// Out-of-range codes deliberately return `""` rather than an error or a
/// placeholder. That is not a fallback for a case that "should not happen" -- it
/// is the documented behaviour of the C macro, which clamps any code below
/// `Z_VERSION_ERROR` or above `Z_NEED_DICT` to slot 9 of the table, an entry
/// that exists solely to hold the empty string. `zError(1000)` returns `""` in
/// the reference implementation and must return `""` here.
///
/// The lookup cannot panic for any input, including [`i32::MIN`], where the
/// naive `2 - code` of the C macro would overflow in Rust's checked arithmetic;
/// see `err_msg_index` for how that is guaranteed.
///
/// Mirrors `zutil.h` L65 (`ERR_MSG`) and `zutil.c` L139-L141 (`zError`).
#[must_use]
pub fn err_msg(code: i32) -> &'static str {
    // `get` rather than `[]`: indexing is denied crate-wide, and an index that
    // somehow fell outside the table must yield the empty string rather than
    // abort a C caller's process. `err_msg_index` only ever returns 0..=9, so
    // the `unwrap_or` arm is unreachable in practice and is here to make the
    // absence of a panicking path structural rather than merely argued.
    Z_ERRMSG.get(err_msg_index(code)).copied().unwrap_or("")
}

/// Maps a raw C `int` onto an index into `Z_ERRMSG`.
///
/// Reproduces the index expression of the `ERR_MSG` macro exactly:
///
/// ```text
/// z_errmsg[(err) < -6 || (err) > 2 ? 9 : 2 - (err)]
/// ```
///
/// The guard is written as a range pattern rather than as the macro's pair of
/// comparisons, which is the same predicate: `Z_VERSION_ERROR ..= Z_NEED_DICT`
/// is precisely the complement of `err < -6 || err > 2`.
///
/// Inside that arm `code` is known to lie in `-6 ..= 2`, so `2 - code` lies in
/// `0 ..= 8` and can neither overflow nor go negative. The subtraction is still
/// written with `i32::checked_sub` and the narrowing with `usize::try_from`, so
/// that the impossibility is enforced by the type system instead of resting on
/// the reader's arithmetic: for `i32::MIN`, which is the input the macro's bare
/// `2 - err` would overflow on, the guard has already selected the sentinel, and
/// even if it had not, `checked_sub` would return [`None`] and the sentinel would
/// be chosen anyway.
///
/// Mirrors `zutil.h` L65 (`ERR_MSG`).
fn err_msg_index(code: i32) -> usize {
    match code {
        // In range: slot `2 - code`, running from 0 for `Z_NEED_DICT` (2) to 8
        // for `Z_VERSION_ERROR` (-6). This is the `2 - (err)` arm of the macro.
        Z_VERSION_ERROR..=Z_NEED_DICT => Z_NEED_DICT
            .checked_sub(code)
            .and_then(|slot| usize::try_from(slot).ok())
            .unwrap_or(OUT_OF_RANGE_INDEX),
        // Out of range -- the macro's `(err) < -6 || (err) > 2` case: slot 9,
        // which holds the empty string.
        _ => OUT_OF_RANGE_INDEX,
    }
}

/// Records `code`'s message in `msg_slot` and returns `code`.
///
/// The direct rendering of the `ERR_RETURN` macro at `zutil.h` L67-L68:
///
/// ```text
/// #define ERR_RETURN(strm,err) \
///   return (strm->msg = ERR_MSG(err), (err))
/// ```
///
/// The argument order matches the macro's -- slot first, code second -- so that
/// a Rust call site reads the same way as the original; `deflate.c`
/// L993, L995, L1020 and L1025 are the call sites this exists for.
/// [`ReturnCode::record_msg`] is the same operation in method form for callers
/// who prefer it, and is what makes this helper reachable from outside the
/// crate.
///
/// As the comment at `zutil.h` L69 puts it, this is "to be used only when the
/// state is known to be valid": it writes to the stream's message slot, which
/// presupposes that the stream has one.
///
/// Mirrors `zutil.h` L67-L68 (`ERR_RETURN`).
#[must_use]
pub(crate) fn err_return(msg_slot: &mut Option<&'static str>, code: ReturnCode) -> ReturnCode {
    *msg_slot = Some(code.msg());
    code
}

#[cfg(test)]
mod tests {
    use super::{err_msg, err_msg_index, err_return, ReturnCode, OUT_OF_RANGE_INDEX, Z_ERRMSG};

    /// Every documented code, paired with the exact `int` that `zlib.h`
    /// L181-L189 assigns to it and the exact string that `zutil.c` L13-L24 pairs
    /// with it.
    ///
    /// The rows are in *table* order rather than numeric order, so a row's
    /// position is also the slot the `ERR_MSG` index formula must select for it.
    const CASES: [(ReturnCode, i32, &str); 9] = [
        (ReturnCode::NEED_DICT, 2, "need dictionary"),
        (ReturnCode::STREAM_END, 1, "stream end"),
        (ReturnCode::OK, 0, ""),
        (ReturnCode::ERRNO, -1, "file error"),
        (ReturnCode::STREAM_ERROR, -2, "stream error"),
        (ReturnCode::DATA_ERROR, -3, "data error"),
        (ReturnCode::MEM_ERROR, -4, "insufficient memory"),
        (ReturnCode::BUF_ERROR, -5, "buffer error"),
        (ReturnCode::VERSION_ERROR, -6, "incompatible version"),
    ];

    /// Integers outside the documented `-6 ..= 2` range, including both extremes
    /// of the type. `i32::MIN` is the input on which the C macro's bare
    /// `2 - (err)` would overflow, and `i32::MAX` its mirror image; both must be
    /// handled, because `zError` accepts any `int`.
    const UNDOCUMENTED: [i32; 10] = [3, 4, 100, 32767, -7, -8, -100, -32768, i32::MIN, i32::MAX];

    #[test]
    fn associated_constants_carry_the_c_integers() {
        for &(code, raw, _) in &CASES {
            assert_eq!(code.as_i32(), raw);
            assert_eq!(i32::from(code), raw);
        }
    }

    #[test]
    fn conversions_round_trip_through_i32() {
        for &(code, raw, _) in &CASES {
            // ReturnCode -> i32 -> ReturnCode
            assert_eq!(ReturnCode::from_i32(code.as_i32()), Some(code));
            // i32 -> ReturnCode -> i32
            assert_eq!(ReturnCode::from_i32(raw).map(ReturnCode::as_i32), Some(raw));
        }
    }

    #[test]
    fn from_i32_rejects_undocumented_codes() {
        for &raw in &UNDOCUMENTED {
            assert_eq!(ReturnCode::from_i32(raw), None);
        }
    }

    #[test]
    fn the_nine_codes_are_distinct() {
        for (i, &(outer, _, _)) in CASES.iter().enumerate() {
            for (j, &(inner, _, _)) in CASES.iter().enumerate() {
                assert_eq!(outer == inner, i == j);
            }
        }
    }

    #[test]
    fn messages_match_the_c_table() {
        for &(code, raw, text) in &CASES {
            assert_eq!(err_msg(raw), text);
            assert_eq!(code.msg(), text);
        }
    }

    #[test]
    fn index_formula_matches_the_c_macro() {
        // A row's position in `CASES` is the slot `2 - code` must select.
        for (slot, &(_, raw, _)) in CASES.iter().enumerate() {
            assert_eq!(err_msg_index(raw), slot);
        }
        for &raw in &UNDOCUMENTED {
            assert_eq!(err_msg_index(raw), OUT_OF_RANGE_INDEX);
        }
    }

    #[test]
    fn table_keeps_the_c_shape() {
        // Exactly ten entries, as `zutil.c` L13 declares and `zutil.h` L62-L63
        // explains; the two empty strings are separate slots and must not be
        // collapsed into one.
        assert_eq!(Z_ERRMSG.len(), 10);
        assert_eq!(Z_ERRMSG.get(2).copied(), Some(""));
        assert_eq!(Z_ERRMSG.get(OUT_OF_RANGE_INDEX).copied(), Some(""));
        assert_eq!(Z_ERRMSG.get(10), None);
    }

    #[test]
    fn boundary_codes_map_to_the_documented_messages() {
        // The two ends of the in-range interval, spelled out as literals so a
        // regression in the guard cannot hide behind the `CASES` table.
        assert_eq!(err_msg(2), "need dictionary");
        assert_eq!(err_msg(-6), "incompatible version");
    }

    #[test]
    fn undocumented_codes_yield_the_empty_sentinel() {
        for &raw in &UNDOCUMENTED {
            assert_eq!(err_msg(raw), "");
        }
        // Called out individually because these are the inputs that a naive
        // `2 - err` would overflow on rather than clamp.
        assert_eq!(err_msg(i32::MIN), "");
        assert_eq!(err_msg(i32::MAX), "");
        // Just outside each boundary.
        assert_eq!(err_msg(3), "");
        assert_eq!(err_msg(-7), "");
    }

    #[test]
    fn is_error_follows_the_sign_rule() {
        for &(code, raw, _) in &CASES {
            assert_eq!(code.is_error(), raw < 0);
        }
        // Positive codes report "special but normal events", per zlib.h L190-L192.
        assert!(!ReturnCode::OK.is_error());
        assert!(!ReturnCode::STREAM_END.is_error());
        assert!(!ReturnCode::NEED_DICT.is_error());
        assert!(ReturnCode::ERRNO.is_error());
        assert!(ReturnCode::VERSION_ERROR.is_error());
    }

    #[test]
    fn err_return_records_the_message_and_yields_the_code() {
        let mut slot = None;
        assert_eq!(
            err_return(&mut slot, ReturnCode::DATA_ERROR),
            ReturnCode::DATA_ERROR
        );
        assert_eq!(slot, Some("data error"));

        // Overwrites whatever was there before, as the C assignment does.
        assert_eq!(
            err_return(&mut slot, ReturnCode::BUF_ERROR),
            ReturnCode::BUF_ERROR
        );
        assert_eq!(slot, Some("buffer error"));
    }

    #[test]
    fn record_msg_is_the_method_form_of_err_return() {
        let mut via_method = Some("a stale message");
        assert_eq!(
            ReturnCode::MEM_ERROR.record_msg(&mut via_method),
            ReturnCode::MEM_ERROR
        );
        assert_eq!(via_method, Some("insufficient memory"));

        let mut direct = Some("a stale message");
        assert_eq!(
            err_return(&mut direct, ReturnCode::MEM_ERROR),
            ReturnCode::MEM_ERROR
        );
        assert_eq!(direct, via_method);
    }

    #[test]
    fn ok_records_the_empty_string_rather_than_null() {
        let mut slot = None;
        assert_eq!(err_return(&mut slot, ReturnCode::OK), ReturnCode::OK);
        // `ERR_MSG(Z_OK)` selects slot 2, which holds "". `None` is the Rust
        // spelling of C's `Z_NULL`, and this deliberately is not that: the C
        // macro always stores a valid pointer.
        assert_eq!(slot, Some(""));
    }
}
