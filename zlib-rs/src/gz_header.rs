//! gzip header metadata (RFC 1952) for `deflateSetHeader` / `inflateGetHeader`.
//!
//! This module defines [`GzHeader`], the idiomatic, safe-Rust model of the C
//! `gz_header` structure declared in `zlib.h` (`struct gz_header_s`). It carries
//! the optional metadata that travels in a gzip member header as specified by
//! [RFC 1952]: the modification time, operating-system code, "extra" field,
//! original file name, comment, and the text / header-CRC flags.
//!
//! # Relationship to the C ABI type
//!
//! The C structure tracks each variable-length buffer with a raw pointer plus a
//! companion length / capacity pair (`extra` + `extra_len` + `extra_max`,
//! `name` + `name_max`, `comment` + `comm_max`). That representation is required
//! only at the `extern "C"` boundary. Here, in the safe core, the buffers are
//! modelled as owned `Option<Vec<u8>>` values: presence (`Some`) replaces the
//! "non-null pointer" test, and `Vec::len` replaces the explicit length field.
//! The capacity (`*_max`) fields exist in C purely to bound how many bytes
//! `inflate` may write into a caller-provided buffer; an owned `Vec` grows as
//! needed, so they have no safe-core analogue and are intentionally dropped.
//!
//! The `#[repr(C)]`, raw-pointer-bearing mirror of this type lives **only** in
//! the `libz-rs-sys` FFI shim, which converts between the two representations
//! under documented `// SAFETY` invariants. This type deliberately contains no
//! `#[repr(C)]` attribute and no raw pointers, in keeping with the crate-wide
//! `#![forbid(unsafe_code)]` policy.
//!
//! [RFC 1952]: https://www.rfc-editor.org/rfc/rfc1952

use alloc::vec::Vec;

/// Operating-system code meaning "unknown", as defined by RFC 1952 §2.3.1.
///
/// RFC 1952 assigns OS code `255` to an unknown / unspecified operating system.
/// It is the value [`GzHeader`]'s [`Default`] implementation uses so that a
/// header constructed without an explicit OS does not falsely advertise a
/// particular platform (the `i32` default of `0` is the "FAT filesystem" code).
pub const OS_UNKNOWN: i32 = 255;

/// gzip header metadata exchanged with `deflateSetHeader` and
/// `inflateGetHeader`.
///
/// `GzHeader` is the safe-core counterpart of the C `gz_header` structure. When
/// compressing, a caller fills in the fields it wants emitted into the gzip
/// member header; when decompressing, the inflate engine populates the fields it
/// parses out of an incoming header and sets [`done`](Self::done) once the whole
/// header has been read.
///
/// All variable-length buffers are stored *without* the C trailing NUL
/// terminator: [`name`](Self::name) and [`comment`](Self::comment) hold only the
/// payload bytes, and the terminator is added or stripped at serialization time
/// by the gzip read / write paths.
///
/// # Examples
///
/// ```ignore
/// let header = GzHeader::new()
///     .with_name(b"archive.tar".to_vec())
///     .with_time(1_700_000_000)
///     .with_os(3); // Unix
/// assert_eq!(header.name_len(), 11);
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GzHeader {
    /// `true` when the uncompressed data is believed to be text (gzip
    /// `FLG.FTEXT`, bit 0). Advisory only; mirrors the C `int text` field.
    pub text: bool,

    /// Modification time of the original file as a Unix timestamp (gzip
    /// `MTIME`). A value of `0` means "no timestamp available" per RFC 1952.
    ///
    /// gzip serializes exactly four little-endian bytes for this field, so a
    /// `u32` represents every value the wire format can carry even though the C
    /// type is the wider `uLong`.
    pub time: u32,

    /// Extra flags (gzip `XFL`). Populated on read; ignored when writing,
    /// because the deflate writer derives `XFL` from the compression level.
    pub xflags: i32,

    /// Operating-system code (gzip `OS`); see [`OS_UNKNOWN`]. A caller's value
    /// is preserved verbatim; [`Default`] uses [`OS_UNKNOWN`].
    pub os: i32,

    /// Optional "extra" field bytes (`FLG.FEXTRA`, bit 2). `None` means the
    /// field is absent; otherwise the raw subfield bytes are stored as-is and
    /// the on-wire `XLEN` equals [`extra_len`](Self::extra_len).
    pub extra: Option<Vec<u8>>,

    /// Optional original file name (`FLG.FNAME`, bit 3), stored **without** the
    /// trailing NUL. Per RFC 1952 the on-wire bytes are ISO 8859-1 (LATIN-1).
    pub name: Option<Vec<u8>>,

    /// Optional human-readable comment (`FLG.FCOMMENT`, bit 4), stored
    /// **without** the trailing NUL.
    pub comment: Option<Vec<u8>>,

    /// `true` if a header CRC-16 was present (on read) or should be emitted (on
    /// write) — `FLG.FHCRC`, bit 1.
    pub hcrc: bool,

    /// `true` once `inflate` has finished reading the entire gzip header.
    /// Unused when writing.
    pub done: bool,
}

impl Default for GzHeader {
    /// Returns an empty header: no optional fields, not text, not done, with a
    /// zero `MTIME` and the operating system set to [`OS_UNKNOWN`].
    ///
    /// `Default` is implemented by hand rather than `#[derive]`d because the
    /// derived implementation would initialize [`os`](Self::os) to `0` (the
    /// `i32` default, i.e. the "FAT filesystem" code), whereas RFC 1952's
    /// neutral value for an unspecified platform is `255` ([`OS_UNKNOWN`]).
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
    /// Creates an empty header equivalent to `GzHeader::default()`.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the [`text`](Self::text) flag and returns the updated header.
    #[must_use]
    pub fn with_text(mut self, text: bool) -> Self {
        self.text = text;
        self
    }

    /// Sets the modification [`time`](Self::time) (gzip `MTIME`) and returns the
    /// updated header.
    #[must_use]
    pub fn with_time(mut self, time: u32) -> Self {
        self.time = time;
        self
    }

    /// Sets the extra-flags ([`xflags`](Self::xflags)) value and returns the
    /// updated header.
    #[must_use]
    pub fn with_xflags(mut self, xflags: i32) -> Self {
        self.xflags = xflags;
        self
    }

    /// Sets the operating-system code ([`os`](Self::os)) and returns the updated
    /// header.
    #[must_use]
    pub fn with_os(mut self, os: i32) -> Self {
        self.os = os;
        self
    }

    /// Sets the [`extra`](Self::extra) field and returns the updated header.
    ///
    /// Accepts anything convertible into a byte vector (for example `Vec<u8>`,
    /// `&[u8]`, or `[u8; N]`).
    #[must_use]
    pub fn with_extra(mut self, extra: impl Into<Vec<u8>>) -> Self {
        self.extra = Some(extra.into());
        self
    }

    /// Sets the original file [`name`](Self::name) (stored without a trailing
    /// NUL) and returns the updated header.
    #[must_use]
    pub fn with_name(mut self, name: impl Into<Vec<u8>>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Sets the [`comment`](Self::comment) (stored without a trailing NUL) and
    /// returns the updated header.
    #[must_use]
    pub fn with_comment(mut self, comment: impl Into<Vec<u8>>) -> Self {
        self.comment = Some(comment.into());
        self
    }

    /// Sets the header-CRC ([`hcrc`](Self::hcrc)) flag and returns the updated
    /// header.
    #[must_use]
    pub fn with_hcrc(mut self, hcrc: bool) -> Self {
        self.hcrc = hcrc;
        self
    }

    /// Length in bytes of the [`extra`](Self::extra) field, or `0` if absent.
    ///
    /// This is the value the FFI shim writes back into the C `extra_len` field,
    /// and equals the on-wire gzip `XLEN`.
    #[must_use]
    pub fn extra_len(&self) -> usize {
        self.extra.as_ref().map_or(0, Vec::len)
    }

    /// Length in bytes of the [`name`](Self::name) field (excluding the trailing
    /// NUL), or `0` if absent.
    #[must_use]
    pub fn name_len(&self) -> usize {
        self.name.as_ref().map_or(0, Vec::len)
    }

    /// Length in bytes of the [`comment`](Self::comment) field (excluding the
    /// trailing NUL), or `0` if absent.
    #[must_use]
    pub fn comment_len(&self) -> usize {
        self.comment.as_ref().map_or(0, Vec::len)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_empty_with_unknown_os() {
        let h = GzHeader::default();
        assert!(!h.text);
        assert_eq!(h.time, 0);
        assert_eq!(h.xflags, 0);
        assert_eq!(h.os, OS_UNKNOWN);
        assert_eq!(h.os, 255);
        assert!(h.extra.is_none());
        assert!(h.name.is_none());
        assert!(h.comment.is_none());
        assert!(!h.hcrc);
        assert!(!h.done);
        // Length accessors report zero for absent buffers.
        assert_eq!(h.extra_len(), 0);
        assert_eq!(h.name_len(), 0);
        assert_eq!(h.comment_len(), 0);
    }

    #[test]
    fn new_equals_default() {
        assert_eq!(GzHeader::new(), GzHeader::default());
    }

    #[test]
    fn builders_set_each_field() {
        let h = GzHeader::new()
            .with_text(true)
            .with_time(1_700_000_000)
            .with_xflags(2)
            .with_os(3)
            .with_extra(b"AB\x01\x02".to_vec())
            .with_name(b"archive.tar".to_vec())
            .with_comment(b"hello".to_vec())
            .with_hcrc(true);

        assert!(h.text);
        assert_eq!(h.time, 1_700_000_000);
        assert_eq!(h.xflags, 2);
        assert_eq!(h.os, 3);
        assert_eq!(h.extra.as_deref(), Some(&b"AB\x01\x02"[..]));
        assert_eq!(h.name.as_deref(), Some(&b"archive.tar"[..]));
        assert_eq!(h.comment.as_deref(), Some(&b"hello"[..]));
        assert!(h.hcrc);
        // `done` is set by the inflate engine, never by a builder.
        assert!(!h.done);
    }

    #[test]
    fn length_accessors_match_buffer_lengths() {
        let h = GzHeader::new()
            .with_extra(b"1234".to_vec())
            .with_name(b"file".to_vec())
            .with_comment(b"comment-text".to_vec());
        assert_eq!(h.extra_len(), 4);
        assert_eq!(h.name_len(), 4);
        assert_eq!(h.comment_len(), 12);
    }

    #[test]
    fn round_trip_set_then_get() {
        let mut h = GzHeader::new();
        h.name = Some(b"name".to_vec());
        h.comment = Some(b"cmt".to_vec());
        h.extra = Some(Vec::from(&b"\x00\xff"[..]));
        h.done = true;

        assert_eq!(h.name.as_deref(), Some(&b"name"[..]));
        assert_eq!(h.comment.as_deref(), Some(&b"cmt"[..]));
        assert_eq!(h.extra.as_deref(), Some(&[0x00_u8, 0xff][..]));
        assert_eq!(h.extra_len(), 2);
        assert!(h.done);
    }

    #[test]
    fn accepts_various_into_vec_sources() {
        // `[u8; N]`, `Vec<u8>`, and `&[u8]` all satisfy `impl Into<Vec<u8>>`.
        let from_array = GzHeader::new().with_name(*b"abc");
        let from_vec = GzHeader::new().with_name(b"abc".to_vec());
        let slice: &[u8] = b"abc";
        let from_slice = GzHeader::new().with_name(slice);

        assert_eq!(from_array, from_vec);
        assert_eq!(from_slice, from_vec);
        assert_eq!(from_vec.name_len(), 3);
    }

    #[test]
    fn clone_and_eq_are_consistent() {
        let h = GzHeader::new().with_name(b"x".to_vec()).with_time(7);
        let c = h.clone();
        assert_eq!(h, c);

        let d = h.clone().with_time(8);
        assert_ne!(h, d);
    }
}
