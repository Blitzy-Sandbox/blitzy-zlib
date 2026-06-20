//! Idiomatic, owned representation of a gzip header (RFC 1952).
//!
//! This module defines [`GzHeader`], the safe-Rust counterpart of the C
//! `gz_header` structure declared in `zlib.h` — the value exchanged with the
//! `deflateSetHeader` and `inflateGetHeader` entry points. It models every
//! piece of gzip metadata described by RFC 1952 (modification time, operating
//! system, the optional *extra*, *name*, and *comment* members, and the text /
//! header-CRC flags) while replacing the C structure's raw `Bytef *` pointers
//! and `*_len` / `*_max` bookkeeping integers with owned, bounds-checked Rust
//! types.
//!
//! # Relationship to the C `gz_header`
//!
//! The C structure (`zlib.h`) carries thirteen fields:
//!
//! ```text
//! int   text;       uLong time;       int   xflags;     int   os;
//! Bytef *extra;     uInt  extra_len;  uInt  extra_max;
//! Bytef *name;      uInt  name_max;   Bytef *comment;   uInt  comm_max;
//! int   hcrc;       int   done;
//! ```
//!
//! The idiomatic form collapses each pointer/length/capacity group into an
//! [`Option`] of an owned byte vector: the `Some` / `None` distinction models
//! the C `extra != Z_NULL` test, and the vector's length supplies the
//! corresponding `*_len`. The read-side capacity integers (`extra_max`,
//! `name_max`, `comm_max`) have **no analogue here**; they exist in C purely to
//! bound writes into caller-supplied fixed buffers, a concern that belongs
//! exclusively to the FFI boundary (see [_FFI marshalling_](#ffi-marshalling)).
//!
//! | C field(s)                                            | Rust field                                   |
//! |-------------------------------------------------------|----------------------------------------------|
//! | `int text`                                            | [`text`](GzHeader::text): [`bool`]           |
//! | `uLong time`                                          | [`time`](GzHeader::time): [`u32`]            |
//! | `int xflags`                                          | [`xflags`](GzHeader::xflags): [`i32`]        |
//! | `int os`                                              | [`os`](GzHeader::os): [`i32`]                |
//! | `Bytef *extra` + `uInt extra_len` + `uInt extra_max`  | [`extra`](GzHeader::extra): `Option<Vec<u8>>`   |
//! | `Bytef *name` + `uInt name_max`                       | [`name`](GzHeader::name): `Option<Vec<u8>>`     |
//! | `Bytef *comment` + `uInt comm_max`                    | [`comment`](GzHeader::comment): `Option<Vec<u8>>` |
//! | `int hcrc`                                            | [`hcrc`](GzHeader::hcrc): [`bool`]           |
//! | `int done`                                            | [`done`](GzHeader::done): [`bool`]           |
//!
//! # Byte-exact compatibility
//!
//! gzip header bytes emitted from a [`GzHeader`] are bit-for-bit identical to
//! those produced by canonical C zlib for the same field values (RFC 1952; see
//! AAP §0.7.1). The [`GzHeader::flags`] helper computes the gzip `FLG` byte
//! exactly as `deflate.c` does, and the field-to-byte mapping for `time`, `os`,
//! `xflags`, `extra`, `name`, `comment`, and `hcrc` is preserved verbatim.
//! Names and comments are stored **without** their terminating NUL byte; the
//! serializer appends the NUL, matching the C convention that a zero byte
//! terminates each string. Consequently a *name* or *comment* must not contain
//! an interior NUL if it is to survive a gzip round trip.
//!
//! # Non-UTF-8 text
//!
//! gzip imposes no character-set requirement on the *name* and *comment*
//! members — they are raw byte strings (nominally ISO-8859-1 per RFC 1952, but
//! in practice arbitrary bytes). They are therefore stored as `Vec<u8>` rather
//! than [`String`]. The [`GzHeader::name_str`] and [`GzHeader::comment_str`]
//! helpers offer a convenient [`Cow`]`<`[`str`]`>` view via
//! [`String::from_utf8_lossy`], which substitutes the Unicode replacement
//! character for invalid sequences and therefore never panics.
//!
//! # FFI marshalling
//!
//! This module contains **no `unsafe` code**. The C-ABI `#[repr(C)] gz_header`
//! (with its raw `*mut Bytef` pointers) and the unsafe pointer marshalling that
//! reads and writes it live in `crate::ffi`. That boundary copies bytes out of
//! a caller's `gz_header` into the owned fields here (on `deflateSetHeader`),
//! and writes this structure's bytes back into the caller's fixed-size buffers
//! up to the `*_max` limits (on `inflateGetHeader`). Keeping the unsafe pointer
//! work in `crate::ffi` confines `unsafe` to the two sanctioned sites in the
//! crate (`crate::ffi` and the `inflate::fast` inner loop), as required by
//! AAP §0.6.2. The public fields and accessors defined here are the safe
//! surface that the FFI layer reads from and writes into.
//!
//! # `no_std`
//!
//! gzip metadata is inherently heap-backed (variable-length names, comments,
//! and extra fields), so this module requires `alloc`. It is compiled only when
//! the `gzip` feature is enabled (`lib.rs` gates it with
//! `#[cfg(feature = "gzip")]`). Under the crate's `no-std` feature it draws
//! `Vec` / `String` / `Cow` from [`alloc`] (via the crate-level
//! `extern crate alloc;`); under the default `std` build the same types come
//! from the prelude.
//!
//! # Examples
//!
//! ```
//! use zlib_rs::gz_header::GzHeader;
//!
//! // A default header is "unknown OS", carries no metadata, and is not yet read.
//! let h = GzHeader::new();
//! assert_eq!(h.os, 255);
//! assert!(h.name.is_none());
//! assert!(!h.done);
//!
//! // Build a header for `deflateSetHeader` with a fluent, owned builder.
//! let h = GzHeader::new()
//!     .with_name(b"hello.txt".to_vec())
//!     .with_comment("a greeting".as_bytes())
//!     .with_time(0x5F00_00FF)
//!     .with_text(true);
//!
//! assert_eq!(h.name_str().unwrap(), "hello.txt");
//! assert!(h.text);
//! ```

#[cfg(not(feature = "std"))]
use alloc::{borrow::Cow, string::String, vec::Vec};
#[cfg(feature = "std")]
use std::borrow::Cow;

use crate::constants::DataType;

/// The gzip "operating system unknown" code (RFC 1952 §2.3.1, OS value `255`).
///
/// This is the default value of [`GzHeader::os`]. It signals that the producing
/// operating system is unspecified. When a gzip stream is written **without** a
/// caller-supplied header, C zlib instead emits the host's compile-time
/// `OS_CODE` (for example `3` for Unix); that substitution is performed by the
/// deflate layer for the *no header* case, whereas a [`GzHeader`] that is
/// supplied to `deflateSetHeader` writes its own [`os`](GzHeader::os) value
/// verbatim (`os & 0xff`).
pub const OS_UNKNOWN: i32 = 255;

/// gzip `FLG` bit `FTEXT` (`0x01`): the file is probably ASCII text.
///
/// Mirrors the bit that `deflate.c` derives from [`GzHeader::text`] and that
/// `inflate.c` reads back into it. See RFC 1952 §2.3.1.
pub const FTEXT: u8 = 0x01;

/// gzip `FLG` bit `FHCRC` (`0x02`): a CRC-16 of the header is present.
///
/// Corresponds to [`GzHeader::hcrc`]. See RFC 1952 §2.3.1.
pub const FHCRC: u8 = 0x02;

/// gzip `FLG` bit `FEXTRA` (`0x04`): an optional *extra* field is present.
///
/// Set whenever [`GzHeader::extra`] is `Some` (independent of its length — an
/// empty-but-present extra field still sets the bit, matching the C
/// `extra != Z_NULL` test). See RFC 1952 §2.3.1.
pub const FEXTRA: u8 = 0x04;

/// gzip `FLG` bit `FNAME` (`0x08`): an original file *name* is present.
///
/// Set whenever [`GzHeader::name`] is `Some`. See RFC 1952 §2.3.1.
pub const FNAME: u8 = 0x08;

/// gzip `FLG` bit `FCOMMENT` (`0x10`): a file *comment* is present.
///
/// Set whenever [`GzHeader::comment`] is `Some`. See RFC 1952 §2.3.1.
pub const FCOMMENT: u8 = 0x10;

/// Owned, safe representation of gzip header metadata (RFC 1952).
///
/// `GzHeader` is the idiomatic mirror of the C `gz_header` structure. It is the
/// value type used by the deflate header-emit path (when a gzip wrapper is
/// requested) and populated by the inflate header-parse path (when the caller
/// has requested header capture). See the [module documentation](self) for the
/// full field-by-field correspondence with the C structure, the byte-exact
/// serialization guarantees, and the FFI marshalling contract.
///
/// # Examples
///
/// ```
/// use zlib_rs::gz_header::GzHeader;
///
/// let h = GzHeader::new().with_name(b"data.bin".to_vec()).with_os(3);
/// assert_eq!(h.os, 3);
/// assert_eq!(h.name.as_deref(), Some(&b"data.bin"[..]));
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GzHeader {
    /// `true` if the uncompressed data is believed to be text rather than
    /// binary. Maps the C `int text` (0/1) to a Rust [`bool`]. Serialized into
    /// the [`FTEXT`] bit of the gzip `FLG` byte.
    pub text: bool,

    /// Modification time of the original file as a Unix timestamp (gzip
    /// `MTIME`), or `0` if no time is available. The gzip wire format stores
    /// `MTIME` as a 4-byte little-endian value, so this is a [`u32`] even
    /// though the C field is declared `uLong`.
    pub time: u32,

    /// Extra flags (gzip `XFL`). These are **not** used when writing — C zlib
    /// derives `XFL` from the compression level — but are filled in from the
    /// header when reading. Kept as [`i32`] to mirror the C `int xflags`.
    pub xflags: i32,

    /// Operating-system code (gzip `OS`). Defaults to [`OS_UNKNOWN`] (`255`).
    /// Written verbatim (`os & 0xff`) when this header is supplied to
    /// `deflateSetHeader`. Kept as [`i32`] to mirror the C `int os`.
    pub os: i32,

    /// Optional *extra* field bytes (gzip `FEXTRA`/`XLEN`), or `None` if absent.
    /// Replaces the C `extra` pointer together with `extra_len`: when `Some`,
    /// the vector holds exactly the extra-field bytes and its length is the
    /// `XLEN` written to / read from the stream. The C read-side `extra_max`
    /// capacity has no analogue here (it lives only at the FFI boundary).
    pub extra: Option<Vec<u8>>,

    /// Optional original file *name* (gzip `FNAME`), or `None` if absent. Stored
    /// as raw bytes **without** the terminating NUL (gzip permits non-UTF-8
    /// names; see [`name_str`](GzHeader::name_str) for a lossy text view). The
    /// C read-side `name_max` capacity has no analogue here.
    pub name: Option<Vec<u8>>,

    /// Optional file *comment* (gzip `FCOMMENT`), or `None` if absent. Stored as
    /// raw bytes **without** the terminating NUL (see
    /// [`comment_str`](GzHeader::comment_str) for a lossy text view). The C
    /// read-side `comm_max` capacity has no analogue here.
    pub comment: Option<Vec<u8>>,

    /// `true` if a header CRC-16 was (or is to be) present. Maps the C
    /// `int hcrc` (0/1) to a [`bool`]; serialized into the [`FHCRC`] bit of the
    /// gzip `FLG` byte.
    pub hcrc: bool,

    /// Read-side completion flag: `true` once the full gzip header has been
    /// consumed by `inflate`. Mirrors the C `int done` for the common
    /// `0` (in progress) / `1` (done) cases. The C tri-state value `-1`
    /// (meaning a *zlib* stream was found, so no gzip header is forthcoming) is
    /// handled at the FFI / inflate boundary, where a `GzHeader` is simply not
    /// populated; it is not represented by this field. Always `false` when
    /// writing.
    pub done: bool,
}

impl Default for GzHeader {
    /// Returns the default gzip header.
    ///
    /// All fields are zero/`false`/`None` **except** [`os`](GzHeader::os),
    /// which defaults to [`OS_UNKNOWN`] (`255`) rather than `0`. This matches
    /// the gzip "unknown operating system" sentinel from RFC 1952. (The deflate
    /// layer substitutes the host `OS_CODE` only for the distinct *no header
    /// supplied at all* case; a constructed `GzHeader` carries `255` until the
    /// caller sets it.)
    ///
    /// Because `os` is non-zero, this implementation is intentionally **not**
    /// `#[derive]`d.
    #[inline]
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
    /// Creates a new default gzip header.
    ///
    /// Equivalent to [`GzHeader::default`]: every field is zero/`false`/`None`
    /// except [`os`](GzHeader::os), which is [`OS_UNKNOWN`] (`255`).
    ///
    /// # Examples
    ///
    /// ```
    /// use zlib_rs::gz_header::GzHeader;
    ///
    /// let h = GzHeader::new();
    /// assert_eq!(h, GzHeader::default());
    /// assert_eq!(h.os, 255);
    /// ```
    #[inline]
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    // -----------------------------------------------------------------------
    // Builder-style setters.
    //
    // Each consumes `self` and returns it, enabling a fluent construction
    // chain that is ergonomic for assembling a header to pass to
    // `deflateSetHeader`. The public fields remain directly assignable for
    // call sites (such as the FFI layer) that hold a `&mut GzHeader`.
    // -----------------------------------------------------------------------

    /// Sets the [`text`](GzHeader::text) flag and returns the updated header.
    #[must_use]
    pub fn with_text(mut self, text: bool) -> Self {
        self.text = text;
        self
    }

    /// Sets the [`time`](GzHeader::time) (gzip `MTIME`) and returns the updated
    /// header.
    #[must_use]
    pub fn with_time(mut self, time: u32) -> Self {
        self.time = time;
        self
    }

    /// Sets the [`os`](GzHeader::os) code and returns the updated header.
    #[must_use]
    pub fn with_os(mut self, os: i32) -> Self {
        self.os = os;
        self
    }

    /// Sets the [`hcrc`](GzHeader::hcrc) flag and returns the updated header.
    #[must_use]
    pub fn with_hcrc(mut self, hcrc: bool) -> Self {
        self.hcrc = hcrc;
        self
    }

    /// Sets the [`extra`](GzHeader::extra) field and returns the updated header.
    ///
    /// Accepts anything convertible into a `Vec<u8>` (for example a `Vec<u8>`
    /// or a `&[u8]`). The presence of an extra field — even an empty one — sets
    /// the [`FEXTRA`] flag bit when the header is serialized.
    #[must_use]
    pub fn with_extra(mut self, extra: impl Into<Vec<u8>>) -> Self {
        self.extra = Some(extra.into());
        self
    }

    /// Sets the [`name`](GzHeader::name) field and returns the updated header.
    ///
    /// Accepts anything convertible into a `Vec<u8>`. The bytes must **not**
    /// include a terminating NUL (it is appended by the serializer) and should
    /// not contain an interior NUL if the name is to survive a gzip round trip.
    #[must_use]
    pub fn with_name(mut self, name: impl Into<Vec<u8>>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Sets the [`comment`](GzHeader::comment) field and returns the updated
    /// header.
    ///
    /// Accepts anything convertible into a `Vec<u8>`. As with
    /// [`with_name`](GzHeader::with_name), the bytes must not include a
    /// terminating NUL.
    #[must_use]
    pub fn with_comment(mut self, comment: impl Into<Vec<u8>>) -> Self {
        self.comment = Some(comment.into());
        self
    }

    // -----------------------------------------------------------------------
    // Accessors / derived views.
    // -----------------------------------------------------------------------

    /// Returns the gzip `FLG` byte for this header.
    ///
    /// The byte is assembled exactly as `deflate.c` does when emitting a gzip
    /// header, OR-ing together [`FTEXT`], [`FHCRC`], [`FEXTRA`], [`FNAME`], and
    /// [`FCOMMENT`] according to the corresponding flags and field presence.
    /// This guarantees byte-identical headers versus canonical C zlib.
    ///
    /// # Examples
    ///
    /// ```
    /// use zlib_rs::gz_header::{GzHeader, FNAME, FTEXT};
    ///
    /// let h = GzHeader::new().with_text(true).with_name(b"f".to_vec());
    /// assert_eq!(h.flags(), FTEXT | FNAME);
    /// ```
    #[must_use]
    pub fn flags(&self) -> u8 {
        let mut flg = 0u8;
        if self.text {
            flg |= FTEXT;
        }
        if self.hcrc {
            flg |= FHCRC;
        }
        if self.extra.is_some() {
            flg |= FEXTRA;
        }
        if self.name.is_some() {
            flg |= FNAME;
        }
        if self.comment.is_some() {
            flg |= FCOMMENT;
        }
        flg
    }

    /// Returns the [`text`](GzHeader::text) flag as a [`DataType`] hint.
    ///
    /// A gzip header that flags its data as text corresponds to
    /// [`DataType::Text`]; otherwise [`DataType::Binary`]. This is the same
    /// binary-versus-text notion that zlib tracks in `z_stream::data_type`, so
    /// the value lines up with the `Z_TEXT` / `Z_BINARY` constants.
    ///
    /// # Examples
    ///
    /// ```
    /// use zlib_rs::constants::DataType;
    /// use zlib_rs::gz_header::GzHeader;
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

    /// Returns a lossy UTF-8 view of the [`name`](GzHeader::name) field, or
    /// `None` if no name is present.
    ///
    /// Because gzip names are arbitrary bytes, the view is produced with
    /// [`String::from_utf8_lossy`]: any byte sequence that is not valid UTF-8 is
    /// rendered using the Unicode replacement character (`U+FFFD`), so this
    /// never panics. The underlying [`name`](GzHeader::name) bytes are left
    /// untouched.
    ///
    /// # Examples
    ///
    /// ```
    /// use zlib_rs::gz_header::GzHeader;
    ///
    /// let h = GzHeader::new().with_name("readme.txt".as_bytes());
    /// assert_eq!(h.name_str().unwrap(), "readme.txt");
    /// assert!(GzHeader::new().name_str().is_none());
    /// ```
    #[must_use]
    pub fn name_str(&self) -> Option<Cow<'_, str>> {
        self.name.as_deref().map(String::from_utf8_lossy)
    }

    /// Returns a lossy UTF-8 view of the [`comment`](GzHeader::comment) field,
    /// or `None` if no comment is present.
    ///
    /// Behaves exactly like [`name_str`](GzHeader::name_str): invalid UTF-8 is
    /// rendered with the replacement character and the call never panics.
    ///
    /// # Examples
    ///
    /// ```
    /// use zlib_rs::gz_header::GzHeader;
    ///
    /// let h = GzHeader::new().with_comment("note".as_bytes());
    /// assert_eq!(h.comment_str().unwrap(), "note");
    /// ```
    #[must_use]
    pub fn comment_str(&self) -> Option<Cow<'_, str>> {
        self.comment.as_deref().map(String::from_utf8_lossy)
    }
}

// ===========================================================================
// Tests — assert default values, builder behavior, byte-exact `FLG`
// computation, lossy text views (including non-UTF-8), and a safe simulation
// of the FFI read-path marshalling round trip.
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_header_matches_c_defaults() {
        let h = GzHeader::default();
        assert!(!h.text, "text defaults to false");
        assert_eq!(h.time, 0, "time defaults to 0 (no mtime)");
        assert_eq!(h.xflags, 0, "xflags defaults to 0");
        assert_eq!(h.os, 255, "os defaults to the RFC 1952 unknown sentinel");
        assert_eq!(h.os, OS_UNKNOWN);
        assert!(h.extra.is_none(), "extra defaults to None");
        assert!(h.name.is_none(), "name defaults to None");
        assert!(h.comment.is_none(), "comment defaults to None");
        assert!(!h.hcrc, "hcrc defaults to false");
        assert!(!h.done, "done defaults to false");
    }

    #[test]
    fn new_equals_default() {
        assert_eq!(GzHeader::new(), GzHeader::default());
    }

    #[test]
    fn flag_and_os_constants_match_rfc1952() {
        assert_eq!(FTEXT, 0x01);
        assert_eq!(FHCRC, 0x02);
        assert_eq!(FEXTRA, 0x04);
        assert_eq!(FNAME, 0x08);
        assert_eq!(FCOMMENT, 0x10);
        assert_eq!(OS_UNKNOWN, 255);
    }

    #[test]
    fn builders_set_fields_and_chain() {
        let h = GzHeader::new()
            .with_text(true)
            .with_time(0x1234_5678)
            .with_os(3)
            .with_hcrc(true)
            .with_extra(vec![1u8, 2, 3])
            .with_name(b"file.bin".to_vec())
            .with_comment("note".as_bytes());

        assert!(h.text);
        assert_eq!(h.time, 0x1234_5678);
        assert_eq!(h.os, 3);
        assert!(h.hcrc);
        assert_eq!(h.extra.as_deref(), Some(&[1u8, 2, 3][..]));
        assert_eq!(h.name.as_deref(), Some(&b"file.bin"[..]));
        assert_eq!(h.comment.as_deref(), Some(&b"note"[..]));
        // Fields that were not set keep their defaults.
        assert_eq!(h.xflags, 0);
        assert!(!h.done);
    }

    #[test]
    fn into_vec_accepts_slice_and_owned_vec() {
        let from_slice = GzHeader::new().with_extra(&[9u8, 8, 7][..]);
        let from_vec = GzHeader::new().with_extra(vec![9u8, 8, 7]);
        assert_eq!(from_slice, from_vec);
        assert_eq!(from_slice.extra.as_deref(), Some(&[9u8, 8, 7][..]));
    }

    #[test]
    fn flags_byte_matches_deflate_emission() {
        // No metadata and no flags -> empty FLG byte.
        assert_eq!(GzHeader::new().flags(), 0);

        // Each presence flips exactly one bit.
        assert_eq!(GzHeader::new().with_text(true).flags(), FTEXT);
        assert_eq!(GzHeader::new().with_hcrc(true).flags(), FHCRC);
        assert_eq!(GzHeader::new().with_extra(vec![0u8]).flags(), FEXTRA);
        assert_eq!(GzHeader::new().with_name(b"n".to_vec()).flags(), FNAME);
        assert_eq!(
            GzHeader::new().with_comment(b"c".to_vec()).flags(),
            FCOMMENT
        );

        // All five together produce 0x1F, exactly as `deflate.c` would.
        let all = GzHeader::new()
            .with_text(true)
            .with_hcrc(true)
            .with_extra(vec![0u8])
            .with_name(b"n".to_vec())
            .with_comment(b"c".to_vec());
        assert_eq!(all.flags(), FTEXT | FHCRC | FEXTRA | FNAME | FCOMMENT);
        assert_eq!(all.flags(), 0x1F);
    }

    #[test]
    fn empty_but_present_extra_still_sets_fextra() {
        // An empty extra field is present, mirroring C's `extra != Z_NULL`
        // test, which is independent of `extra_len`.
        let h = GzHeader::new().with_extra(Vec::new());
        assert!(h.extra.is_some());
        assert_eq!(h.extra.as_deref(), Some(&[][..]));
        assert_eq!(h.flags() & FEXTRA, FEXTRA);
    }

    #[test]
    fn data_type_tracks_text_flag() {
        assert_eq!(GzHeader::new().data_type(), DataType::Binary);
        assert_eq!(GzHeader::new().with_text(true).data_type(), DataType::Text);
        // The hint matches the raw Z_TEXT / Z_BINARY constants.
        assert_eq!(i32::from(GzHeader::new().data_type()), 0);
        assert_eq!(i32::from(GzHeader::new().with_text(true).data_type()), 1);
    }

    #[test]
    fn name_and_comment_str_roundtrip_utf8() {
        let h = GzHeader::new()
            .with_name("réadme.txt".as_bytes())
            .with_comment("héllo".as_bytes());
        assert_eq!(h.name_str().unwrap(), "réadme.txt");
        assert_eq!(h.comment_str().unwrap(), "héllo");
    }

    #[test]
    fn name_and_comment_str_are_none_when_absent() {
        assert!(GzHeader::new().name_str().is_none());
        assert!(GzHeader::new().comment_str().is_none());
    }

    #[test]
    fn lossy_str_views_do_not_panic_on_invalid_utf8() {
        // 0xFF is never a valid UTF-8 byte; 0x80/0x81 are stray continuation
        // bytes. `from_utf8_lossy` substitutes U+FFFD for each and must not
        // panic.
        let h = GzHeader::new()
            .with_name(vec![b'a', 0xFF, b'b'])
            .with_comment(vec![0x80, 0x81]);

        let name = h.name_str().expect("name present");
        let comment = h.comment_str().expect("comment present");

        assert!(name.contains('\u{FFFD}'));
        assert!(name.starts_with('a'));
        assert!(name.ends_with('b'));
        assert!(comment.chars().all(|c| c == '\u{FFFD}'));

        // The raw bytes are preserved untouched by the lossy view.
        assert_eq!(h.name.as_deref(), Some(&[b'a', 0xFF, b'b'][..]));
        assert_eq!(h.comment.as_deref(), Some(&[0x80u8, 0x81][..]));
    }

    #[test]
    fn clone_and_equality() {
        let h = GzHeader::new()
            .with_name(b"a.txt".to_vec())
            .with_time(42)
            .with_text(true);
        let cloned = h.clone();
        assert_eq!(h, cloned);

        let mut changed = h.clone();
        changed.time = 43;
        assert_ne!(h, changed);
    }

    #[test]
    fn ffi_read_path_marshalling_simulation() {
        // Simulate, using only safe operations, what `crate::ffi` performs on
        // the inflate read path: it copies bytes out of the caller's C buffers
        // into the owned fields, dropping the terminating NUL from name and
        // comment and recording the actual extra-field length. This proves the
        // safe representation captures every datum the C `gz_header` carries.
        //
        // Pretend the C side handed us these raw, NUL-terminated string buffers
        // and a (length-prefixed on the wire) extra field:
        let c_name = b"input.txt\0";
        let c_comment = b"created by zlib-rs\0";
        let c_extra = [0xDEu8, 0xAD, 0xBE, 0xEF];

        let mut h = GzHeader::new();
        h.text = true;
        h.time = 0x6500_1234;
        h.xflags = 0;
        h.os = 3;
        h.hcrc = true;
        // Strip the trailing NUL exactly as the reader does.
        h.name = Some(c_name[..c_name.len() - 1].to_vec());
        h.comment = Some(c_comment[..c_comment.len() - 1].to_vec());
        h.extra = Some(c_extra.to_vec());
        h.done = true;

        // The owned structure round-trips back to the same logical content.
        assert_eq!(h.name_str().unwrap(), "input.txt");
        assert_eq!(h.comment_str().unwrap(), "created by zlib-rs");

        let extra = h.extra.as_deref().expect("extra present");
        assert_eq!(extra, &c_extra[..]);
        assert_eq!(extra.len(), 4, "extra_len is recovered from Vec::len");

        assert!(h.done);
        assert_eq!(h.flags(), FTEXT | FHCRC | FEXTRA | FNAME | FCOMMENT);

        // A full clone compares equal (deterministic, owned data only).
        assert_eq!(h, h.clone());
    }

    #[test]
    fn debug_impl_is_available() {
        // The `Debug` derive must be present (it powers `assert_eq!`
        // diagnostics and is part of the public contract).
        let rendered = format!("{:?}", GzHeader::new().with_name(b"x".to_vec()));
        assert!(rendered.contains("GzHeader"));
        assert!(rendered.contains("name"));
    }
}
