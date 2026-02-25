// Copyright (C) 1995-2026 Jean-loup Gailly and Mark Adler
// Rust translation: zlib-rs crate
// SPDX-License-Identifier: Zlib
//
// This module provides the GzHeader struct for gzip header metadata,
// equivalent to the C gz_header struct defined in zlib.h (lines 118-135).

//! Gzip header metadata types and OS identifier constants.
//!
//! This module provides the [`GzHeader`] struct, which carries gzip header
//! information as defined in [RFC 1952](https://datatracker.ietf.org/doc/html/rfc1952).
//! It is used with `deflate_set_header` to set custom gzip header fields during
//! compression, and with `inflate_get_header` to retrieve gzip header information
//! during decompression.
//!
//! The [`os`] submodule provides constants for the operating system identifier
//! field per RFC 1952 §2.3.1.
//!
//! # Examples
//!
//! Creating a GzHeader with builder methods:
//!
//! ```
//! use zlib_rs::gz_header::{GzHeader, os};
//!
//! let header = GzHeader::new()
//!     .with_name("example.txt")
//!     .with_comment("compressed by zlib-rs")
//!     .with_time(1700000000)
//!     .with_os(os::UNIX);
//!
//! assert_eq!(header.name.as_deref(), Some("example.txt"));
//! assert_eq!(header.comment.as_deref(), Some("compressed by zlib-rs"));
//! assert_eq!(header.time, 1700000000);
//! assert_eq!(header.os, os::UNIX);
//! ```

/// Operating system identifiers for the gzip header OS field.
///
/// These constants are defined in RFC 1952 §2.3.1. The OS field in a gzip
/// header identifies the type of file system on which the compression
/// took place. This can be useful to determine end-of-line convention
/// for text files.
///
/// # Values
///
/// | Value | OS Name |
/// |-------|---------|
/// | 0 | FAT filesystem (MS-DOS, OS/2, NT/Win32) |
/// | 1 | Amiga |
/// | 2 | VMS (or OpenVMS) |
/// | 3 | Unix |
/// | 4 | VM/CMS |
/// | 5 | Atari TOS |
/// | 6 | HPFS filesystem (OS/2, NT) |
/// | 7 | Macintosh |
/// | 8 | Z-System |
/// | 9 | CP/M |
/// | 10 | TOPS-20 |
/// | 11 | NTFS filesystem (NT) |
/// | 12 | QDOS |
/// | 13 | Acorn RISCOS |
/// | 255 | Unknown |
pub mod os {
    /// FAT filesystem (MS-DOS, OS/2, NT/Win32).
    pub const FAT: i32 = 0;

    /// Amiga operating system.
    pub const AMIGA: i32 = 1;

    /// VMS (or OpenVMS) operating system.
    pub const VMS: i32 = 2;

    /// Unix operating system (includes Linux, macOS, BSDs, etc.).
    pub const UNIX: i32 = 3;

    /// VM/CMS operating system (IBM mainframe).
    pub const VM_CMS: i32 = 4;

    /// Atari TOS operating system.
    pub const ATARI: i32 = 5;

    /// HPFS filesystem (OS/2, NT).
    pub const HPFS: i32 = 6;

    /// Macintosh operating system (classic Mac OS).
    pub const MACINTOSH: i32 = 7;

    /// Z-System operating system.
    pub const Z_SYSTEM: i32 = 8;

    /// CP/M operating system.
    pub const CP_M: i32 = 9;

    /// TOPS-20 operating system (DEC PDP-10).
    pub const TOPS20: i32 = 10;

    /// NTFS filesystem (Windows NT).
    pub const NTFS: i32 = 11;

    /// QDOS operating system (Sinclair QL).
    pub const QDOS: i32 = 12;

    /// Acorn RISCOS operating system.
    pub const RISCOS: i32 = 13;

    /// Unknown operating system.
    ///
    /// Used when the OS cannot be determined, or for cross-platform
    /// compressed data where the source OS is irrelevant.
    pub const UNKNOWN: i32 = 255;
}

/// Gzip header metadata, equivalent to C zlib's `gz_header`.
///
/// This struct carries gzip header information as defined in
/// [RFC 1952](https://datatracker.ietf.org/doc/html/rfc1952).
/// It is used with `deflate_set_header` to set custom gzip header fields
/// during compression, and with `inflate_get_header` to retrieve gzip header
/// information during decompression.
///
/// # C-to-Rust Translation
///
/// The original C `gz_header` struct (defined in `zlib.h`) uses raw pointers
/// with separate length/capacity fields for optional data. This Rust version
/// replaces those patterns with idiomatic owned types:
///
/// | C Field | Rust Equivalent | Notes |
/// |---------|-----------------|-------|
/// | `Bytef *extra` + `extra_len` + `extra_max` | `Option<Vec<u8>>` | Owns the extra data |
/// | `Bytef *name` + `name_max` | `Option<String>` | Owns the file name |
/// | `Bytef *comment` + `comm_max` | `Option<String>` | Owns the comment |
/// | `int text` | `bool` | `0/1` → `false/true` |
/// | `int hcrc` | `bool` | `0/1` → `false/true` |
/// | `uLong time` | `u32` | Unix timestamp fits `u32` |
///
/// The `extra_max`, `name_max`, and `comm_max` fields from the C struct are
/// eliminated because Rust's `Vec` and `String` types manage their own
/// capacity internally.
///
/// # Gzip Header Format (RFC 1952 §2.3)
///
/// The gzip header consists of:
/// - A 10-byte mandatory header with identification, compression method,
///   flags, modification time, extra flags, and OS identifier
/// - Optional fields indicated by flags: extra data (FEXTRA), original
///   file name (FNAME), comment (FCOMMENT), and header CRC16 (FHCRC)
///
/// # Examples
///
/// Creating a default (empty) header:
///
/// ```
/// use zlib_rs::gz_header::GzHeader;
///
/// let header = GzHeader::new();
/// assert!(!header.text);
/// assert_eq!(header.time, 0);
/// assert!(header.extra.is_none());
/// assert!(header.name.is_none());
/// assert!(header.comment.is_none());
/// ```
///
/// Using the builder pattern:
///
/// ```
/// use zlib_rs::gz_header::{GzHeader, os};
///
/// let header = GzHeader::new()
///     .with_name("data.bin")
///     .with_time(1700000000)
///     .with_os(os::UNIX)
///     .with_extra(vec![0x01, 0x02, 0x03, 0x04]);
///
/// assert_eq!(header.extra_len(), 4);
/// assert_eq!(header.os, os::UNIX);
/// ```
#[derive(Debug, Clone, Default)]
pub struct GzHeader {
    /// True if the compressed data is believed to be text.
    ///
    /// Corresponds to the FTEXT flag (bit 0) in the gzip header's FLG byte.
    /// This is a hint that can be used by text-mode transport layers to
    /// perform end-of-line conversion. It has no effect on compression
    /// or decompression.
    ///
    /// Defaults to `false`.
    pub text: bool,

    /// Modification time of the original file as a Unix timestamp.
    ///
    /// This is the time in seconds since 00:00:00 GMT, Jan 1, 1970.
    /// A value of `0` means no time stamp is available. This field is
    /// stored as a 4-byte little-endian value in the gzip header (MTIME).
    ///
    /// Defaults to `0`.
    pub time: u32,

    /// Extra flags indicating the compression level used.
    ///
    /// Not used when writing a gzip file (set by the compressor for
    /// informational purposes). When reading:
    /// - `2` indicates the compressor used maximum compression (slowest algorithm)
    /// - `4` indicates the compressor used fastest compression
    ///
    /// This corresponds to the XFL byte in the gzip header.
    ///
    /// Defaults to `0`.
    pub xflags: i32,

    /// Operating system identifier for the file system on which compression
    /// took place.
    ///
    /// See the [`os`] module for predefined values per RFC 1952 §2.3.1.
    /// This corresponds to the OS byte in the gzip header.
    ///
    /// Defaults to `0` (FAT filesystem).
    pub os: i32,

    /// Optional extra field data.
    ///
    /// `None` if no extra field is present. When set, contains arbitrary
    /// extra data that is stored in the gzip header's XLEN+extra area
    /// (indicated by the FEXTRA flag, bit 2 of FLG).
    ///
    /// In the C zlib API, this was a raw `Bytef *` pointer with a separate
    /// `extra_len` field. In Rust, the `Vec<u8>` owns the data and tracks
    /// its length internally.
    ///
    /// Defaults to `None`.
    pub extra: Option<Vec<u8>>,

    /// Optional original file name.
    ///
    /// `None` if no file name is present. When set, contains the original
    /// file name of the compressed file, stored as a zero-terminated string
    /// in the gzip header (indicated by the FNAME flag, bit 3 of FLG).
    ///
    /// The name should be in ISO 8859-1 (Latin-1) per RFC 1952, but this
    /// implementation uses a Rust `String` (UTF-8) for ergonomics. Callers
    /// should ensure names are representable in Latin-1 for strict compliance.
    ///
    /// In the C zlib API, this was a raw `Bytef *` pointer with a separate
    /// `name_max` field.
    ///
    /// Defaults to `None`.
    pub name: Option<String>,

    /// Optional comment string.
    ///
    /// `None` if no comment is present. When set, contains a human-readable
    /// comment stored as a zero-terminated string in the gzip header
    /// (indicated by the FCOMMENT flag, bit 4 of FLG).
    ///
    /// The comment should be in ISO 8859-1 (Latin-1) per RFC 1952, but this
    /// implementation uses a Rust `String` (UTF-8) for ergonomics.
    ///
    /// In the C zlib API, this was a raw `Bytef *` pointer with a separate
    /// `comm_max` field.
    ///
    /// Defaults to `None`.
    pub comment: Option<String>,

    /// True if there was or will be a header CRC16.
    ///
    /// Corresponds to the FHCRC flag (bit 1) in the gzip header's FLG byte.
    /// When true, a CRC16 of the gzip header is present immediately before
    /// the compressed data. This CRC16 consists of the two least significant
    /// bytes of the CRC-32 of all header bytes up to and including the
    /// header CRC16 field.
    ///
    /// Defaults to `false`.
    pub hcrc: bool,

    /// True when done reading the gzip header during decompression.
    ///
    /// This is an internal flag used by the inflate engine to track whether
    /// the gzip header has been fully parsed. It is not used when writing
    /// a gzip file. Applications should not need to set this field directly.
    ///
    /// Defaults to `false`.
    #[allow(dead_code)]
    pub(crate) done: bool,
}

impl GzHeader {
    /// Creates a new empty `GzHeader` with all fields set to their defaults.
    ///
    /// All `Option` fields are `None`, boolean fields are `false`, and
    /// numeric fields are `0`. This is equivalent to `GzHeader::default()`.
    ///
    /// # Examples
    ///
    /// ```
    /// use zlib_rs::gz_header::GzHeader;
    ///
    /// let header = GzHeader::new();
    /// assert!(!header.text);
    /// assert_eq!(header.time, 0);
    /// assert_eq!(header.xflags, 0);
    /// assert_eq!(header.os, 0);
    /// assert!(header.extra.is_none());
    /// assert!(header.name.is_none());
    /// assert!(header.comment.is_none());
    /// assert!(!header.hcrc);
    /// ```
    #[inline]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the length of the extra field data, or `0` if no extra field
    /// is present.
    ///
    /// This is equivalent to the C `extra_len` field, but computed from the
    /// owned `Vec<u8>` rather than stored separately.
    ///
    /// # Examples
    ///
    /// ```
    /// use zlib_rs::gz_header::GzHeader;
    ///
    /// let header = GzHeader::new();
    /// assert_eq!(header.extra_len(), 0);
    ///
    /// let header = GzHeader::new().with_extra(vec![1, 2, 3]);
    /// assert_eq!(header.extra_len(), 3);
    /// ```
    #[inline]
    pub fn extra_len(&self) -> usize {
        self.extra.as_ref().map_or(0, |v| v.len())
    }

    /// Sets the original file name using a builder-style method.
    ///
    /// Accepts any type that implements `Into<String>`, including `&str`
    /// and `String`. The name is stored as `Some(name)`.
    ///
    /// Per RFC 1952, the file name should contain only ISO 8859-1 (Latin-1)
    /// characters and should not include a directory path. The name is
    /// stored as a zero-terminated string in the gzip header.
    ///
    /// # Examples
    ///
    /// ```
    /// use zlib_rs::gz_header::GzHeader;
    ///
    /// let header = GzHeader::new().with_name("archive.dat");
    /// assert_eq!(header.name.as_deref(), Some("archive.dat"));
    ///
    /// // Also works with owned String
    /// let name = String::from("output.bin");
    /// let header = GzHeader::new().with_name(name);
    /// assert_eq!(header.name.as_deref(), Some("output.bin"));
    /// ```
    #[inline]
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Sets the comment string using a builder-style method.
    ///
    /// Accepts any type that implements `Into<String>`, including `&str`
    /// and `String`. The comment is stored as `Some(comment)`.
    ///
    /// Per RFC 1952, the comment should contain only ISO 8859-1 (Latin-1)
    /// characters. It is intended for human consumption and is not
    /// interpreted by decompression software.
    ///
    /// # Examples
    ///
    /// ```
    /// use zlib_rs::gz_header::GzHeader;
    ///
    /// let header = GzHeader::new()
    ///     .with_comment("Compressed with zlib-rs v1.3.2.1");
    /// assert_eq!(
    ///     header.comment.as_deref(),
    ///     Some("Compressed with zlib-rs v1.3.2.1")
    /// );
    /// ```
    #[inline]
    pub fn with_comment(mut self, comment: impl Into<String>) -> Self {
        self.comment = Some(comment.into());
        self
    }

    /// Sets the extra field data using a builder-style method.
    ///
    /// The extra field allows storing arbitrary application-specific data
    /// in the gzip header. Per RFC 1952 §2.3.1.1, the extra field consists
    /// of subfields, each with a two-byte SI1/SI2 identifier, a two-byte
    /// length, and the subfield data.
    ///
    /// # Examples
    ///
    /// ```
    /// use zlib_rs::gz_header::GzHeader;
    ///
    /// // Extra field with a subfield: SI1=0x41 ('A'), SI2=0x70 ('p'), len=2, data=[0x01, 0x02]
    /// let extra = vec![0x41, 0x70, 0x02, 0x00, 0x01, 0x02];
    /// let header = GzHeader::new().with_extra(extra);
    /// assert_eq!(header.extra_len(), 6);
    /// ```
    #[inline]
    pub fn with_extra(mut self, extra: Vec<u8>) -> Self {
        self.extra = Some(extra);
        self
    }

    /// Sets the modification time using a builder-style method.
    ///
    /// The time is a Unix timestamp (seconds since 1970-01-01 00:00:00 UTC).
    /// A value of `0` means no time stamp is available. The timestamp is
    /// stored as a 4-byte little-endian value in the gzip MTIME field.
    ///
    /// # Examples
    ///
    /// ```
    /// use zlib_rs::gz_header::GzHeader;
    ///
    /// // Set modification time to 2023-11-14 22:13:20 UTC
    /// let header = GzHeader::new().with_time(1700000000);
    /// assert_eq!(header.time, 1700000000);
    /// ```
    #[inline]
    pub fn with_time(mut self, time: u32) -> Self {
        self.time = time;
        self
    }

    /// Sets the OS identifier using a builder-style method.
    ///
    /// The OS field identifies the type of file system on which compression
    /// took place. Use constants from the [`os`] module for predefined values.
    ///
    /// # Examples
    ///
    /// ```
    /// use zlib_rs::gz_header::{GzHeader, os};
    ///
    /// let header = GzHeader::new().with_os(os::UNIX);
    /// assert_eq!(header.os, os::UNIX);
    /// assert_eq!(header.os, 3);
    ///
    /// let header = GzHeader::new().with_os(os::UNKNOWN);
    /// assert_eq!(header.os, 255);
    /// ```
    #[inline]
    pub fn with_os(mut self, os: i32) -> Self {
        self.os = os;
        self
    }

    /// Returns `true` if the header indicates this is the end of header
    /// parsing during decompression.
    ///
    /// This is an internal method used by the inflate engine. Applications
    /// generally do not need to call this directly.
    #[inline]
    #[allow(dead_code)]
    pub(crate) fn is_done(&self) -> bool {
        self.done
    }

    /// Marks the header as done (used internally by the inflate engine).
    #[inline]
    #[allow(dead_code)]
    pub(crate) fn set_done(&mut self, done: bool) {
        self.done = done;
    }
}

/// Implements `PartialEq` for `GzHeader` comparing all public fields.
///
/// The internal `done` field is excluded from equality comparison as it
/// represents transient parsing state, not header content.
impl PartialEq for GzHeader {
    fn eq(&self, other: &Self) -> bool {
        self.text == other.text
            && self.time == other.time
            && self.xflags == other.xflags
            && self.os == other.os
            && self.extra == other.extra
            && self.name == other.name
            && self.comment == other.comment
            && self.hcrc == other.hcrc
    }
}

impl Eq for GzHeader {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_values() {
        let header = GzHeader::new();
        assert!(!header.text);
        assert_eq!(header.time, 0);
        assert_eq!(header.xflags, 0);
        assert_eq!(header.os, 0);
        assert!(header.extra.is_none());
        assert!(header.name.is_none());
        assert!(header.comment.is_none());
        assert!(!header.hcrc);
        assert!(!header.done);
    }

    #[test]
    fn test_new_equals_default() {
        let from_new = GzHeader::new();
        let from_default = GzHeader::default();
        assert_eq!(from_new, from_default);
    }

    #[test]
    fn test_extra_len_none() {
        let header = GzHeader::new();
        assert_eq!(header.extra_len(), 0);
    }

    #[test]
    fn test_extra_len_some() {
        let header = GzHeader::new().with_extra(vec![1, 2, 3, 4, 5]);
        assert_eq!(header.extra_len(), 5);
    }

    #[test]
    fn test_extra_len_empty_vec() {
        let header = GzHeader::new().with_extra(vec![]);
        assert_eq!(header.extra_len(), 0);
        assert!(header.extra.is_some()); // Some(vec![]) vs None
    }

    #[test]
    fn test_with_name() {
        let header = GzHeader::new().with_name("test.txt");
        assert_eq!(header.name.as_deref(), Some("test.txt"));
    }

    #[test]
    fn test_with_name_owned_string() {
        let name = String::from("owned_name.dat");
        let header = GzHeader::new().with_name(name);
        assert_eq!(header.name.as_deref(), Some("owned_name.dat"));
    }

    #[test]
    fn test_with_comment() {
        let header = GzHeader::new().with_comment("a comment");
        assert_eq!(header.comment.as_deref(), Some("a comment"));
    }

    #[test]
    fn test_with_extra() {
        let data = vec![0xDE, 0xAD, 0xBE, 0xEF];
        let header = GzHeader::new().with_extra(data.clone());
        assert_eq!(header.extra, Some(data));
    }

    #[test]
    fn test_with_time() {
        let header = GzHeader::new().with_time(1700000000);
        assert_eq!(header.time, 1700000000);
    }

    #[test]
    fn test_with_time_zero() {
        let header = GzHeader::new().with_time(0);
        assert_eq!(header.time, 0);
    }

    #[test]
    fn test_with_time_max() {
        let header = GzHeader::new().with_time(u32::MAX);
        assert_eq!(header.time, u32::MAX);
    }

    #[test]
    fn test_with_os() {
        let header = GzHeader::new().with_os(os::UNIX);
        assert_eq!(header.os, 3);
    }

    #[test]
    fn test_builder_chaining() {
        let header = GzHeader::new()
            .with_name("data.bin")
            .with_comment("compressed data")
            .with_extra(vec![0x01, 0x02])
            .with_time(1700000000)
            .with_os(os::UNIX);

        assert_eq!(header.name.as_deref(), Some("data.bin"));
        assert_eq!(header.comment.as_deref(), Some("compressed data"));
        assert_eq!(header.extra, Some(vec![0x01, 0x02]));
        assert_eq!(header.time, 1700000000);
        assert_eq!(header.os, os::UNIX);
    }

    #[test]
    fn test_clone() {
        let original = GzHeader::new()
            .with_name("cloned.txt")
            .with_extra(vec![1, 2, 3]);
        let cloned = original.clone();
        assert_eq!(original, cloned);
    }

    #[test]
    fn test_debug_format() {
        let header = GzHeader::new();
        let debug_str = format!("{:?}", header);
        assert!(debug_str.contains("GzHeader"));
        assert!(debug_str.contains("text: false"));
        assert!(debug_str.contains("time: 0"));
    }

    #[test]
    fn test_equality_ignores_done() {
        let mut header1 = GzHeader::new().with_name("test.txt");
        let mut header2 = GzHeader::new().with_name("test.txt");
        header1.done = true;
        header2.done = false;
        // done is excluded from equality comparison
        assert_eq!(header1, header2);
    }

    #[test]
    fn test_inequality_different_names() {
        let header1 = GzHeader::new().with_name("a.txt");
        let header2 = GzHeader::new().with_name("b.txt");
        assert_ne!(header1, header2);
    }

    #[test]
    fn test_inequality_name_vs_none() {
        let header1 = GzHeader::new().with_name("a.txt");
        let header2 = GzHeader::new();
        assert_ne!(header1, header2);
    }

    #[test]
    fn test_os_constants() {
        assert_eq!(os::FAT, 0);
        assert_eq!(os::AMIGA, 1);
        assert_eq!(os::VMS, 2);
        assert_eq!(os::UNIX, 3);
        assert_eq!(os::VM_CMS, 4);
        assert_eq!(os::ATARI, 5);
        assert_eq!(os::HPFS, 6);
        assert_eq!(os::MACINTOSH, 7);
        assert_eq!(os::Z_SYSTEM, 8);
        assert_eq!(os::CP_M, 9);
        assert_eq!(os::TOPS20, 10);
        assert_eq!(os::NTFS, 11);
        assert_eq!(os::QDOS, 12);
        assert_eq!(os::RISCOS, 13);
        assert_eq!(os::UNKNOWN, 255);
    }

    #[test]
    fn test_os_constants_match_rfc1952() {
        // RFC 1952 §2.3.1 OS field values
        // Verify all 15 defined values are distinct and in expected ranges
        let all_values = [
            os::FAT,
            os::AMIGA,
            os::VMS,
            os::UNIX,
            os::VM_CMS,
            os::ATARI,
            os::HPFS,
            os::MACINTOSH,
            os::Z_SYSTEM,
            os::CP_M,
            os::TOPS20,
            os::NTFS,
            os::QDOS,
            os::RISCOS,
            os::UNKNOWN,
        ];

        // All values should be unique
        for i in 0..all_values.len() {
            for j in (i + 1)..all_values.len() {
                assert_ne!(
                    all_values[i], all_values[j],
                    "OS constants at indices {} and {} have duplicate value {}",
                    i, j, all_values[i]
                );
            }
        }

        // All values except UNKNOWN should be in 0..=13 range
        for &val in &all_values[..14] {
            assert!(
                (0..=13).contains(&val),
                "OS constant {} out of expected 0..=13 range",
                val
            );
        }

        // UNKNOWN must be 255
        assert_eq!(all_values[14], 255);
    }

    #[test]
    fn test_text_field() {
        let mut header = GzHeader::new();
        assert!(!header.text);
        header.text = true;
        assert!(header.text);
    }

    #[test]
    fn test_hcrc_field() {
        let mut header = GzHeader::new();
        assert!(!header.hcrc);
        header.hcrc = true;
        assert!(header.hcrc);
    }

    #[test]
    fn test_xflags_field() {
        let mut header = GzHeader::new();
        assert_eq!(header.xflags, 0);
        header.xflags = 2; // Maximum compression indicator
        assert_eq!(header.xflags, 2);
        header.xflags = 4; // Fastest compression indicator
        assert_eq!(header.xflags, 4);
    }

    #[test]
    fn test_is_done_and_set_done() {
        let mut header = GzHeader::new();
        assert!(!header.is_done());
        header.set_done(true);
        assert!(header.is_done());
        header.set_done(false);
        assert!(!header.is_done());
    }

    #[test]
    fn test_large_extra_field() {
        // Gzip extra field length is stored as 16-bit, max 65535 bytes
        let large_extra = vec![0xAB; 65535];
        let header = GzHeader::new().with_extra(large_extra);
        assert_eq!(header.extra_len(), 65535);
    }

    #[test]
    fn test_unicode_name() {
        // While RFC 1952 specifies Latin-1, our Rust String supports UTF-8
        let header = GzHeader::new().with_name("données.txt");
        assert_eq!(header.name.as_deref(), Some("données.txt"));
    }

    #[test]
    fn test_all_fields_set() {
        let header = GzHeader {
            text: true,
            time: 1234567890,
            xflags: 2,
            os: os::UNIX,
            extra: Some(vec![0x01, 0x02]),
            name: Some(String::from("test.gz")),
            comment: Some(String::from("test comment")),
            hcrc: true,
            done: false,
        };

        assert!(header.text);
        assert_eq!(header.time, 1234567890);
        assert_eq!(header.xflags, 2);
        assert_eq!(header.os, 3);
        assert_eq!(header.extra_len(), 2);
        assert_eq!(header.name.as_deref(), Some("test.gz"));
        assert_eq!(header.comment.as_deref(), Some("test comment"));
        assert!(header.hcrc);
        assert!(!header.done);
    }
}
