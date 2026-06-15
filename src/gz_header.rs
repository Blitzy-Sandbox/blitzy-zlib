//! Idiomatic, owned representation of a **gzip header** (RFC 1952).
//!
//! This module defines [`GzHeader`], the safe-Rust analogue of the C
//! `gz_header` structure declared in `zlib.h` (lines 117-131). A `GzHeader`
//! carries the optional metadata that travels in the gzip member header —
//! modification time, originating operating system, an optional *extra* field,
//! an optional file *name*, an optional *comment*, and the text/header-CRC
//! flags. It is the value exchanged with the two public zlib entry points that
//! deal with gzip metadata:
//!
//! * **`deflateSetHeader`** (write side) — the caller fills in a `GzHeader`
//!   and the deflate engine serializes it ahead of the compressed data.
//! * **`inflateGetHeader`** (read side) — the inflate engine parses the gzip
//!   header from the stream and populates a `GzHeader` for the caller.
//!
//! # Relationship to the C `gz_header`
//!
//! The C structure mixes data pointers with companion length/capacity integers
//! and uses `Z_NULL` pointers as "absent" sentinels:
//!
//! ```c
//! typedef struct gz_header_s {
//!     int     text;       /* true if compressed data believed to be text */
//!     uLong   time;       /* modification time */
//!     int     xflags;     /* extra flags (not used when writing a gzip file) */
//!     int     os;         /* operating system */
//!     Bytef   *extra;     /* pointer to extra field or Z_NULL if none */
//!     uInt    extra_len;  /* extra field length (valid if extra != Z_NULL) */
//!     uInt    extra_max;  /* space at extra (only when reading header) */
//!     Bytef   *name;      /* pointer to zero-terminated file name or Z_NULL */
//!     uInt    name_max;   /* space at name (only when reading header) */
//!     Bytef   *comment;   /* pointer to zero-terminated comment or Z_NULL */
//!     uInt    comm_max;   /* space at comment (only when reading header) */
//!     int     hcrc;       /* true if there was or will be a header crc */
//!     int     done;       /* true when done reading gzip header */
//! } gz_header;
//! ```
//!
//! The idiomatic form collapses each `pointer + length (+ capacity)` triple
//! into a single owning [`Option<Vec<u8>>`], so the wire-level semantics are
//! expressed directly by the Rust type system:
//!
//! | C field(s)                          | Rust field              | Mapping notes |
//! |-------------------------------------|-------------------------|---------------|
//! | `int text`                          | [`text: bool`]          | gzip `FTEXT` flag (bit 0) |
//! | `uLong time`                        | [`time: u32`]           | `MTIME`, 4 bytes little-endian; `0` = no timestamp |
//! | `int xflags`                        | [`xflags: i32`]         | `XFL` byte; **read-only** (see below) |
//! | `int os`                            | [`os: i32`]             | `OS` byte; default [`OS_UNKNOWN`] (`255`) |
//! | `Bytef *extra` + `uInt extra_len`   | [`extra: Option<Vec<u8>>`] | `None` ⇔ `Z_NULL` (no `FEXTRA`); length is `Vec::len` |
//! | `uInt extra_max`                    | *(handled in `ffi.rs`)* | read-side buffer capacity |
//! | `Bytef *name`                       | [`name: Option<Vec<u8>>`]  | `None` ⇔ `Z_NULL` (no `FNAME`); bytes **exclude** the trailing NUL |
//! | `uInt name_max`                     | *(handled in `ffi.rs`)* | read-side buffer capacity |
//! | `Bytef *comment`                    | [`comment: Option<Vec<u8>>`] | `None` ⇔ `Z_NULL` (no `FCOMMENT`); bytes **exclude** the trailing NUL |
//! | `uInt comm_max`                     | *(handled in `ffi.rs`)* | read-side buffer capacity |
//! | `int hcrc`                          | [`hcrc: bool`]          | gzip `FHCRC` flag (bit 1) |
//! | `int done`                          | [`done: bool`]          | read side: `true` once the header is fully parsed |
//!
//! [`text: bool`]: GzHeader::text
//! [`time: u32`]: GzHeader::time
//! [`xflags: i32`]: GzHeader::xflags
//! [`os: i32`]: GzHeader::os
//! [`extra: Option<Vec<u8>>`]: GzHeader::extra
//! [`name: Option<Vec<u8>>`]: GzHeader::name
//! [`comment: Option<Vec<u8>>`]: GzHeader::comment
//! [`hcrc: bool`]: GzHeader::hcrc
//! [`done: bool`]: GzHeader::done
//!
//! ## "Absent" semantics ([`Option`] vs `Z_NULL`)
//!
//! In a gzip header the presence of an extra field, file name, or comment is
//! signalled by a flag bit (`FEXTRA`/`FNAME`/`FCOMMENT`) in the `FLG` byte. C
//! zlib decides each bit from whether the corresponding pointer is non-`NULL`
//! (`deflate.c`: `extra == Z_NULL ? 0 : 4`, etc.). The [`Option`] modeling here
//! reproduces that decision exactly and unambiguously:
//!
//! * `None`            → the field is absent; its flag bit is **clear**.
//! * `Some(bytes)`     → the field is present; its flag bit is **set**.
//! * `Some(Vec::new())`→ the field is present but **empty** (an extra field with
//!   `XLEN == 0`, or a name/comment that is just the terminating NUL) — exactly
//!   matching a non-`NULL` C pointer to a zero-length payload.
//!
//! ## Byte-exact serialization contract (RFC 1952)
//!
//! `GzHeader` is purely a data holder; the actual bytes are emitted by the
//! deflate engine (`crate::deflate`) and parsed by the inflate engine
//! (`crate::inflate`). To keep gzip headers **byte-identical** to canonical C
//! zlib, those engines observe the following rules, which this type is designed
//! to support:
//!
//! * `time` is written as four little-endian bytes (`MTIME`).
//! * `os` is written as a single byte (`os & 0xff`); the conventional default
//!   for portable output is [`OS_UNKNOWN`] (`255`).
//! * `xflags` (`XFL`) is **derived from the compression level/strategy when
//!   writing** and is therefore ignored on the write path — the C comment notes
//!   it is "not used when writing a gzip file". It is populated only on the read
//!   path from the parsed `XFL` byte. Hence there is intentionally no
//!   `with_xflags` builder.
//! * `extra`, when present, is written as a two-byte little-endian length
//!   (`XLEN`, capped at `0xffff`) followed by the bytes.
//! * `name` and `comment`, when present, are written verbatim **followed by a
//!   single NUL terminator** that this type does *not* store. They must not
//!   contain interior NUL bytes, matching C's NUL-terminated `Bytef *`
//!   semantics (a NUL would prematurely terminate the field on the wire).
//! * `hcrc`, when set, causes a two-byte little-endian CRC-16 of the header to
//!   be appended (`FHCRC`).
//!
//! # FFI boundary
//!
//! This module is the **safe** representation only. The C-ABI
//! `#[repr(C)] gz_header` (raw `*mut Bytef` pointers plus the `*_len`/`*_max`
//! companions) and *all* of the `unsafe` pointer marshalling live in
//! `crate::ffi`. See [the FFI conversion contract](#ffi-conversion-contract)
//! below for the exact division of responsibility. **There is no `unsafe` code
//! in this file** (per AAP §0.6.2, `unsafe` is confined to `ffi.rs` and
//! `inflate/fast.rs`).
//!
//! ## FFI conversion contract
//!
//! Marshalling between the safe [`GzHeader`] and the C-ABI
//! `#[repr(C)] gz_header` is owned **entirely** by `crate::ffi`; this module
//! deliberately exposes no conversion functions (doing so would require it to
//! name the raw `#[repr(C)]` type, which is not in this file's dependency set,
//! and would pull `unsafe` pointer handling into the safe core). The contract
//! that `crate::ffi` implements over this type's public fields is:
//!
//! * **Write side — `deflateSetHeader` (C → Rust).** Read each field of the
//!   caller's `#[repr(C)] gz_header` and build a [`GzHeader`]:
//!   - `text`/`hcrc` from the C `int`s (`!= 0`);
//!   - `time`/`xflags`/`os` copied directly (`time` narrowed to [`u32`]);
//!   - for `extra`: if the C `extra` pointer is `Z_NULL`, [`GzHeader::extra`]
//!     is `None`; otherwise it is `Some` of the `extra_len` bytes copied out
//!     of the pointer;
//!   - for `name`/`comment`: if the C pointer is `Z_NULL`, the field is `None`;
//!     otherwise it is `Some` of the bytes up to (but not including) the
//!     terminating NUL.
//!
//! * **Read side — `inflateGetHeader` (Rust → C).** After the inflate engine
//!   fills a [`GzHeader`], write it back into the caller's
//!   `#[repr(C)] gz_header`, honouring the read-only capacity companions:
//!   - copy at most `extra_max` bytes of [`GzHeader::extra`] into `extra` and
//!     set `extra_len` to the true (untruncated) length;
//!   - copy at most `name_max` bytes of [`GzHeader::name`] into `name`,
//!     appending a NUL terminator within the available space; likewise for
//!     `comment`/`comm_max`;
//!   - copy back `text`, `time`, `xflags`, `os`, `hcrc`, and set `done`.
//!
//! All raw-pointer dereferences, bounds checks against `*_max`, and C-string
//! handling are performed with `unsafe` **in `crate::ffi`**. This module's only
//! obligation is to model the data faithfully (which it does via the
//! [`Option`]/[`Vec`] fields and the accessors above) so that the marshalling
//! is a straightforward, mechanical copy.
//!
//! # `no_std`
//!
//! `GzHeader` owns heap data ([`Vec`]/[`String`]), so it requires an allocator.
//! Under the default `std` build those types come from the standard library;
//! under a `no_std` build the module is reached only with the `gzip` feature
//! enabled and pulls the same types from the `alloc` crate (which the crate root
//! brings into scope via `extern crate alloc;`). The crate root gates the module
//! with `#[cfg(feature = "gzip")] pub mod gz_header;`.
//!
//! # Examples
//!
//! Build a header for `deflateSetHeader`:
//!
//! ```
//! use zlib_rs::gz_header::GzHeader;
//!
//! let header = GzHeader::new()
//!     .with_name("example.txt")
//!     .with_comment("written by zlib-rs")
//!     .with_time(1_700_000_000)
//!     .with_text(true);
//!
//! assert_eq!(header.name_str().as_deref(), Some("example.txt"));
//! assert!(header.has_name());
//! // A freshly constructed header reports the RFC 1952 "unknown" OS.
//! assert_eq!(header.os, 255);
//! ```

// ===========================================================================
// Imports
// ===========================================================================
//
// Heap-backed container/string types used by the owned representation. Under
// the default `std` build, `Vec` and `String` are in the prelude and only
// `Cow` needs importing (from `std::borrow`). Under a `no_std` + `gzip` build
// they all come from the `alloc` crate, which `lib.rs` brings into scope with
// `extern crate alloc;`.
#[cfg(not(feature = "std"))]
use alloc::{borrow::Cow, string::String, vec::Vec};
#[cfg(feature = "std")]
use std::borrow::Cow;

// The only cross-module dependency (the file's dependency whitelist is
// `src/constants.rs` alone). `DataType` lets [`GzHeader::data_type`] express the
// gzip `FTEXT` flag in terms of the crate's canonical data-type classification.
use crate::constants::DataType;

// ===========================================================================
// Public constants tied to the `os` / `xflags` fields (RFC 1952)
// ===========================================================================

/// The gzip "operating system unknown" code (`0xff` = `255`), per RFC 1952.
///
/// This is the default value of [`GzHeader::os`] and the conventional choice
/// for portable, reproducible output: it records that the originating operating
/// system is unspecified rather than leaking the host platform. (Compare the C
/// `OS_CODE` macro in `zutil.h`, which defaults to `3` for Unix; canonical zlib
/// writes `OS_CODE` only when *no* header is supplied, whereas a caller-supplied
/// header carries whatever `os` value it was given — `255` here by default.)
pub const OS_UNKNOWN: i32 = 255;

/// `XFL` value meaning "maximum compression, slowest algorithm" (RFC 1952).
///
/// Provided for interpreting [`GzHeader::xflags`] on the read path. The write
/// path computes `XFL` from the compression level/strategy, so this is a
/// read-side reference value.
pub const XFL_MAX_COMPRESSION: i32 = 2;

/// `XFL` value meaning "fastest algorithm" (RFC 1952).
///
/// Provided for interpreting [`GzHeader::xflags`] on the read path. See
/// [`XFL_MAX_COMPRESSION`] for the write-side note.
pub const XFL_FASTEST: i32 = 4;

// ===========================================================================
// The idiomatic header
// ===========================================================================

/// Owned gzip header metadata (RFC 1952) — the safe analogue of C `gz_header`.
///
/// All fields are public so the deflate header-emit path
/// (`crate::deflate`), the inflate header-parse path (`crate::inflate`), and the
/// FFI marshalling layer (`crate::ffi`) can read and write them directly. The
/// builder methods ([`with_name`](GzHeader::with_name) and friends) and the
/// convenience accessors ([`name_str`](GzHeader::name_str),
/// [`has_extra`](GzHeader::has_extra), [`data_type`](GzHeader::data_type), …)
/// offer an ergonomic surface on top of those fields.
///
/// `Default`/[`new`](GzHeader::new) produce an all-empty header whose `os` is
/// [`OS_UNKNOWN`] (`255`); every other field is `false`/`0`/`None`.
///
/// See the [module documentation](crate::gz_header) for the full field-to-byte
/// mapping and the [`Option`]-vs-`Z_NULL` absence semantics.
///
/// # Examples
///
/// ```
/// use zlib_rs::gz_header::GzHeader;
///
/// // Absent fields are `None`; an empty-but-present field is `Some(vec![])`.
/// let mut h = GzHeader::new();
/// assert!(h.name.is_none());          // no FNAME
/// h.name = Some(Vec::new());          // FNAME set, name is just the NUL
/// assert!(h.has_name());
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GzHeader {
    /// Whether the compressed data is believed to be text (gzip `FTEXT` flag).
    ///
    /// Maps the C `int text` (`0`/`1`) to a `bool`. See
    /// [`data_type`](GzHeader::data_type) for the corresponding
    /// [`DataType`] classification.
    pub text: bool,

    /// Modification time as a Unix timestamp (gzip `MTIME`).
    ///
    /// Serialized as four little-endian bytes. A value of `0` means "no
    /// timestamp available" per RFC 1952. The C field is `uLong`, but the gzip
    /// `MTIME` field is exactly four bytes, so [`u32`] is the precise type.
    pub time: u32,

    /// Extra flags byte (gzip `XFL`).
    ///
    /// Populated on the **read** path from the parsed `XFL` byte (see
    /// [`XFL_MAX_COMPRESSION`] / [`XFL_FASTEST`]). On the **write** path zlib
    /// derives `XFL` from the compression level/strategy and ignores this
    /// field, mirroring the C comment "not used when writing a gzip file".
    pub xflags: i32,

    /// Originating operating-system code (gzip `OS`).
    ///
    /// Serialized as a single byte (`os & 0xff`). Defaults to [`OS_UNKNOWN`]
    /// (`255`).
    pub os: i32,

    /// Optional *extra* field bytes (gzip `FEXTRA`).
    ///
    /// `None` means no extra field (the `FEXTRA` flag is clear, matching a
    /// `Z_NULL` C pointer). `Some(bytes)` sets `FEXTRA`; the on-the-wire `XLEN`
    /// is `bytes.len()` truncated to 16 bits (`0xffff` maximum), so callers
    /// should keep extra fields within `65_535` bytes.
    pub extra: Option<Vec<u8>>,

    /// Optional original file name (gzip `FNAME`).
    ///
    /// Stored as raw bytes **without** the trailing NUL terminator (gzip names
    /// are not guaranteed to be UTF-8). `None` means no name (`FNAME` clear);
    /// `Some(bytes)` sets `FNAME` and the serializer appends the NUL. Use
    /// [`name_str`](GzHeader::name_str) for a lossy `str` view.
    pub name: Option<Vec<u8>>,

    /// Optional file comment (gzip `FCOMMENT`).
    ///
    /// Stored as raw bytes **without** the trailing NUL terminator. `None`
    /// means no comment (`FCOMMENT` clear); `Some(bytes)` sets `FCOMMENT` and
    /// the serializer appends the NUL. Use
    /// [`comment_str`](GzHeader::comment_str) for a lossy `str` view.
    pub comment: Option<Vec<u8>>,

    /// Whether a header CRC is (or will be) present (gzip `FHCRC` flag).
    ///
    /// When `true` on the write path, a two-byte little-endian CRC-16 of the
    /// header bytes is appended; on the read path it reflects the parsed
    /// `FHCRC` bit.
    pub hcrc: bool,

    /// Read-side completion flag: `true` once the entire gzip header has been
    /// consumed by the inflate engine.
    ///
    /// Not used on the write path. The C field is an `int` that also takes an
    /// intermediate `-1` ("header parsing in progress") value; that transient
    /// state is tracked internally by the inflate state machine, so the
    /// idiomatic surface exposes only the terminal `true`/`false` distinction.
    pub done: bool,
}

impl Default for GzHeader {
    /// Returns an all-empty header whose `os` is [`OS_UNKNOWN`] (`255`).
    ///
    /// `Default` is implemented by hand rather than derived precisely because
    /// the `os` field must default to `255` (RFC 1952 "unknown"); a derived
    /// `Default` would yield `os == 0`, which is a *valid* OS code (FAT
    /// filesystem) and would therefore silently change the emitted header byte.
    /// Every other field takes its natural zero value (`false`/`0`/`None`).
    fn default() -> Self {
        Self {
            text: false,
            time: 0,
            xflags: 0,
            os: OS_UNKNOWN,
            extra: None,
            name: None,
            comment: None,
            hcrc: false,
            done: false,
        }
    }
}

impl GzHeader {
    // -----------------------------------------------------------------------
    // Construction
    // -----------------------------------------------------------------------

    /// Creates a new, empty header (identical to [`GzHeader::default`]).
    ///
    /// The result has `text == false`, `time == 0`, `xflags == 0`,
    /// `os == 255` ([`OS_UNKNOWN`]), `extra`/`name`/`comment == None`,
    /// `hcrc == false`, and `done == false`.
    ///
    /// # Examples
    ///
    /// ```
    /// use zlib_rs::gz_header::GzHeader;
    ///
    /// let h = GzHeader::new();
    /// assert_eq!(h.os, 255);
    /// assert!(h.extra.is_none() && h.name.is_none() && h.comment.is_none());
    /// assert!(!h.text && !h.hcrc && !h.done);
    /// ```
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    // -----------------------------------------------------------------------
    // Builder-style setters (consuming `self`, chainable)
    //
    // These return `Self` so a header can be assembled fluently, e.g. for
    // `deflateSetHeader`. Each is `#[must_use]` because dropping the returned
    // value would discard the mutation.
    // -----------------------------------------------------------------------

    /// Sets the [`text`](GzHeader::text) flag (gzip `FTEXT`).
    #[must_use]
    pub fn with_text(mut self, text: bool) -> Self {
        self.text = text;
        self
    }

    /// Sets the modification [`time`](GzHeader::time) (gzip `MTIME`).
    ///
    /// Use `0` to indicate "no timestamp".
    #[must_use]
    pub fn with_time(mut self, time: u32) -> Self {
        self.time = time;
        self
    }

    /// Sets the originating operating-system code ([`os`](GzHeader::os)).
    ///
    /// Pass [`OS_UNKNOWN`] (`255`) for portable, host-independent output.
    #[must_use]
    pub fn with_os(mut self, os: i32) -> Self {
        self.os = os;
        self
    }

    /// Sets the [`name`](GzHeader::name) field (gzip `FNAME`).
    ///
    /// Accepts anything convertible into a byte vector (`&str`, [`String`],
    /// `&[u8]`, [`Vec<u8>`], …). The bytes must **not** include a trailing or
    /// interior NUL: the serializer appends the single terminating NUL, and an
    /// interior NUL would truncate the name on the wire.
    ///
    /// # Examples
    ///
    /// ```
    /// use zlib_rs::gz_header::GzHeader;
    ///
    /// let h = GzHeader::new().with_name("photo.png");
    /// assert_eq!(h.name.as_deref(), Some(&b"photo.png"[..]));
    /// ```
    #[must_use]
    pub fn with_name(mut self, name: impl Into<Vec<u8>>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Sets the [`comment`](GzHeader::comment) field (gzip `FCOMMENT`).
    ///
    /// Accepts anything convertible into a byte vector. As with
    /// [`with_name`](GzHeader::with_name), the bytes must not contain a NUL;
    /// the terminating NUL is added by the serializer.
    #[must_use]
    pub fn with_comment(mut self, comment: impl Into<Vec<u8>>) -> Self {
        self.comment = Some(comment.into());
        self
    }

    /// Sets the [`extra`](GzHeader::extra) field (gzip `FEXTRA`).
    ///
    /// Accepts anything convertible into a byte vector. The on-the-wire `XLEN`
    /// is the length truncated to 16 bits, so keep the payload within
    /// `65_535` bytes.
    #[must_use]
    pub fn with_extra(mut self, extra: impl Into<Vec<u8>>) -> Self {
        self.extra = Some(extra.into());
        self
    }

    /// Sets the [`hcrc`](GzHeader::hcrc) flag (gzip `FHCRC`).
    ///
    /// When `true`, the deflate engine appends a two-byte CRC-16 of the header.
    #[must_use]
    pub fn with_hcrc(mut self, hcrc: bool) -> Self {
        self.hcrc = hcrc;
        self
    }

    // -----------------------------------------------------------------------
    // Convenience accessors
    // -----------------------------------------------------------------------

    /// Returns the file name as a lossy UTF-8 string, or `None` if absent.
    ///
    /// gzip names are arbitrary bytes and need not be valid UTF-8, so this uses
    /// [`String::from_utf8_lossy`]: invalid sequences are replaced with the
    /// Unicode replacement character (`U+FFFD`) and the call never panics. The
    /// returned [`Cow`] borrows when the bytes are already valid UTF-8 and
    /// allocates only when replacement is required.
    ///
    /// # Examples
    ///
    /// ```
    /// use zlib_rs::gz_header::GzHeader;
    ///
    /// let h = GzHeader::new().with_name(vec![b'h', b'i', 0xff]);
    /// // The invalid 0xFF byte becomes the replacement character.
    /// assert_eq!(h.name_str().as_deref(), Some("hi\u{fffd}"));
    /// ```
    #[must_use]
    pub fn name_str(&self) -> Option<Cow<'_, str>> {
        self.name.as_deref().map(String::from_utf8_lossy)
    }

    /// Returns the comment as a lossy UTF-8 string, or `None` if absent.
    ///
    /// Behaves exactly like [`name_str`](GzHeader::name_str) but for the
    /// [`comment`](GzHeader::comment) field.
    #[must_use]
    pub fn comment_str(&self) -> Option<Cow<'_, str>> {
        self.comment.as_deref().map(String::from_utf8_lossy)
    }

    /// Returns the length in bytes of the extra field, or `0` if absent.
    ///
    /// This is the idiomatic counterpart of the C `extra_len` companion field
    /// (which collapses into the [`Vec`] length here).
    #[must_use]
    pub fn extra_len(&self) -> usize {
        self.extra.as_ref().map_or(0, Vec::len)
    }

    /// Returns `true` if an extra field is present (gzip `FEXTRA` will be set).
    ///
    /// Equivalent to the C `extra != Z_NULL` test that drives the `FEXTRA`
    /// flag bit. Note that an empty-but-present extra field
    /// (`Some(Vec::new())`) still counts as present.
    #[must_use]
    pub fn has_extra(&self) -> bool {
        self.extra.is_some()
    }

    /// Returns `true` if a file name is present (gzip `FNAME` will be set).
    ///
    /// Equivalent to the C `name != Z_NULL` test.
    #[must_use]
    pub fn has_name(&self) -> bool {
        self.name.is_some()
    }

    /// Returns `true` if a comment is present (gzip `FCOMMENT` will be set).
    ///
    /// Equivalent to the C `comment != Z_NULL` test.
    #[must_use]
    pub fn has_comment(&self) -> bool {
        self.comment.is_some()
    }

    /// Returns the [`DataType`] classification implied by the
    /// [`text`](GzHeader::text) flag.
    ///
    /// The gzip `FTEXT` bit corresponds directly to zlib's text/binary
    /// classification: a set flag maps to [`DataType::Text`] (`Z_TEXT`) and a
    /// clear flag to [`DataType::Binary`] (`Z_BINARY`). Because `text` is a
    /// boolean, this never returns [`DataType::Unknown`].
    ///
    /// # Examples
    ///
    /// ```
    /// use zlib_rs::gz_header::GzHeader;
    /// use zlib_rs::constants::DataType;
    ///
    /// assert_eq!(GzHeader::new().data_type(), DataType::Binary);
    /// assert_eq!(GzHeader::new().with_text(true).data_type(), DataType::Text);
    /// ```
    #[must_use]
    pub fn data_type(&self) -> DataType {
        if self.text {
            DataType::Text
        } else {
            DataType::Binary
        }
    }

    /// Resets the header to its [`Default`] state (all-empty, `os == 255`).
    ///
    /// Useful for reusing a single `GzHeader` across multiple
    /// `inflateGetHeader` calls without reallocating it, mirroring how the C
    /// inflate engine clears `done` before parsing a new member header.
    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::constants::{Z_BINARY, Z_TEXT};

    /// A freshly-constructed header must be all-empty with `os == 255`. This is
    /// the explicit AAP validation case ("default header has `os==255`, all
    /// options `None`, `text==false`, `done==false`").
    #[test]
    fn default_header_is_empty_with_os_unknown() {
        let h = GzHeader::default();
        assert_eq!(h.os, 255);
        assert_eq!(h.os, OS_UNKNOWN);
        assert!(!h.text);
        assert_eq!(h.time, 0);
        assert_eq!(h.xflags, 0);
        assert!(h.extra.is_none());
        assert!(h.name.is_none());
        assert!(h.comment.is_none());
        assert!(!h.hcrc);
        assert!(!h.done);
    }

    /// `new()` must be identical to `default()` — both yield `os == 255`. This
    /// guards against the derived-`Default` footgun (which would give
    /// `os == 0`) and keeps the two constructors in lockstep.
    #[test]
    fn new_equals_default() {
        assert_eq!(GzHeader::new(), GzHeader::default());
        assert_eq!(GzHeader::new().os, 255);
    }

    /// The public OS/XFL reference constants carry their RFC 1952 values.
    #[test]
    fn header_constants_match_rfc1952() {
        assert_eq!(OS_UNKNOWN, 255);
        assert_eq!(XFL_MAX_COMPRESSION, 2);
        assert_eq!(XFL_FASTEST, 4);
    }

    /// Each builder sets exactly its own field, and the builders chain.
    #[test]
    fn builders_set_fields_and_chain() {
        let h = GzHeader::new()
            .with_text(true)
            .with_time(1_700_000_000)
            .with_os(3)
            .with_name("file.bin")
            .with_comment("a comment")
            .with_extra(vec![1, 2, 3, 4])
            .with_hcrc(true);

        assert!(h.text);
        assert_eq!(h.time, 1_700_000_000);
        assert_eq!(h.os, 3);
        assert_eq!(h.name.as_deref(), Some(&b"file.bin"[..]));
        assert_eq!(h.comment.as_deref(), Some(&b"a comment"[..]));
        assert_eq!(h.extra.as_deref(), Some(&[1u8, 2, 3, 4][..]));
        assert!(h.hcrc);
    }

    /// `with_name`/`with_comment`/`with_extra` accept every `Into<Vec<u8>>`
    /// source (`&str`, `String`, `Vec<u8>`, `&[u8]`).
    #[test]
    fn builders_accept_varied_into_vec_sources() {
        let from_str = GzHeader::new().with_name("abc");
        let from_string = GzHeader::new().with_name(String::from("abc"));
        let from_vec = GzHeader::new().with_name(vec![b'a', b'b', b'c']);
        let from_slice = GzHeader::new().with_name(&b"abc"[..]);

        for h in [&from_str, &from_string, &from_vec, &from_slice] {
            assert_eq!(h.name.as_deref(), Some(&b"abc"[..]));
        }
        assert_eq!(from_str, from_string);
        assert_eq!(from_str, from_vec);
        assert_eq!(from_str, from_slice);
    }

    /// `name_str` borrows for valid UTF-8 and is `None` when the name is absent.
    #[test]
    fn name_str_valid_utf8_and_absent() {
        let present = GzHeader::new().with_name("héllo.txt");
        assert_eq!(present.name_str().as_deref(), Some("héllo.txt"));
        // Valid UTF-8 should be borrowed, not reallocated.
        assert!(matches!(present.name_str(), Some(Cow::Borrowed(_))));

        let absent = GzHeader::new();
        assert!(absent.name_str().is_none());
    }

    /// `name_str`/`comment_str` must replace invalid UTF-8 without panicking,
    /// returning an owned (allocated) `Cow`.
    #[test]
    fn str_views_handle_non_utf8_without_panic() {
        // 0xFF is never valid UTF-8.
        let h = GzHeader::new()
            .with_name(vec![b'n', 0xff, b'm'])
            .with_comment(vec![0xfe, 0xff]);

        assert_eq!(h.name_str().as_deref(), Some("n\u{fffd}m"));
        assert_eq!(h.comment_str().as_deref(), Some("\u{fffd}\u{fffd}"));
        // Replacement forces an allocation, so the variant is `Owned`.
        assert!(matches!(h.name_str(), Some(Cow::Owned(_))));
    }

    /// `extra_len` mirrors the C `extra_len` companion: `0` when absent, the
    /// byte length when present (including the empty-but-present case).
    #[test]
    fn extra_len_reports_byte_length() {
        assert_eq!(GzHeader::new().extra_len(), 0);
        assert_eq!(GzHeader::new().with_extra(vec![]).extra_len(), 0);
        assert_eq!(GzHeader::new().with_extra(vec![9, 9, 9]).extra_len(), 3);
    }

    /// The `has_*` predicates reproduce the C `field != Z_NULL` tests that drive
    /// the gzip `FEXTRA`/`FNAME`/`FCOMMENT` flag bits, including the crucial
    /// `Some(empty)` (present-but-empty) vs `None` (absent) distinction.
    #[test]
    fn has_predicates_track_presence_not_emptiness() {
        let absent = GzHeader::new();
        assert!(!absent.has_extra());
        assert!(!absent.has_name());
        assert!(!absent.has_comment());

        // `Some(Vec::new())` is *present* (non-NULL pointer to empty payload).
        let empty_present = GzHeader::new()
            .with_extra(Vec::new())
            .with_name(Vec::new())
            .with_comment(Vec::new());
        assert!(empty_present.has_extra());
        assert!(empty_present.has_name());
        assert!(empty_present.has_comment());

        // Present-but-empty must not equal absent: the flag bits differ.
        assert_ne!(empty_present, absent);
    }

    /// `data_type` bridges the `FTEXT` flag to the crate's `DataType`, and the
    /// numeric round-trip matches the raw `Z_TEXT`/`Z_BINARY` wire values.
    #[test]
    fn data_type_bridges_text_flag() {
        let binary = GzHeader::new();
        let text = GzHeader::new().with_text(true);

        assert_eq!(binary.data_type(), DataType::Binary);
        assert_eq!(text.data_type(), DataType::Text);
        assert_eq!(i32::from(binary.data_type()), Z_BINARY);
        assert_eq!(i32::from(text.data_type()), Z_TEXT);
    }

    /// `reset` returns a mutated header to the default state.
    #[test]
    fn reset_restores_default() {
        let mut h = GzHeader::new()
            .with_text(true)
            .with_os(7)
            .with_name("x")
            .with_hcrc(true);
        h.done = true;

        h.reset();
        assert_eq!(h, GzHeader::default());
        assert_eq!(h.os, 255);
        assert!(h.name.is_none());
        assert!(!h.done);
    }

    /// `Clone`/`PartialEq`/`Eq` behave structurally across all fields.
    #[test]
    fn clone_and_equality_are_structural() {
        let original = GzHeader::new()
            .with_name("dup.txt")
            .with_extra(vec![0xde, 0xad])
            .with_time(42)
            .with_text(true);
        let cloned = original.clone();
        assert_eq!(original, cloned);

        let mut different = original.clone();
        different.time = 43;
        assert_ne!(original, different);
    }

    /// `Debug` is derivable and renders the struct name (smoke test).
    #[test]
    fn debug_formatting_is_available() {
        let dbg = format!("{:?}", GzHeader::new().with_name("z"));
        assert!(dbg.contains("GzHeader"));
        assert!(dbg.contains("name"));
    }
}
