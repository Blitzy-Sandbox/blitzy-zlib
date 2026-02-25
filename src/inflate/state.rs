//! Inflate state machine types and state struct.
//!
//! Ported from C zlib's `inflate.h` (inflate_mode enum, inflate_state struct)
//! and related type definitions from `inftrees.h`. This module defines the
//! [`InflateState`] struct holding all decompression state (~35 fields), and
//! the [`InflateMode`] enum with 32 variants representing every state in the
//! inflate state machine.
//!
//! # Design Decisions
//!
//! - C pointer fields (`lencode`, `distcode`) are replaced by `usize` indices
//!   (`lencode_idx`, `distcode_idx`) into the `codes` Vec for memory safety.
//! - The sliding `window` is a `Vec<u8>` with lazy allocation (empty until first use).
//! - The `codes` buffer is a `Vec<Code>` pre-allocated to `ENOUGH` (1444) entries.
//! - The gzip header reference (`head`) uses `Option<Box<GzHeader>>` instead of
//!   a raw `gz_headerp` pointer.
//! - The bit accumulator (`hold`) uses `u64` for comfortable bit manipulation.

// All items in this module are used by sibling inflate modules (mod.rs, fast.rs,
// back.rs) that are being created in parallel. Suppress dead-code warnings until
// those modules are in place.
#![allow(dead_code)]

use core::fmt;

// In no_std mode, pull alloc types that the std prelude normally provides.
#[cfg(not(feature = "std"))]
use alloc::{boxed::Box, string::String, vec, vec::Vec};

use super::fixed::{DISTFIX, FIXDCODES_BITS, FIXLCODES_BITS, LENFIX};
use super::tables::{Code, ENOUGH};
use crate::error::ZlibError;
use crate::gz_header::GzHeader;

// ---------------------------------------------------------------------------
// InflateMode — inflate state machine modes
// ---------------------------------------------------------------------------

/// State machine modes for the inflate decompression engine.
///
/// These modes correspond to the processing stages of zlib, gzip, and raw
/// DEFLATE streams. The [`inflate()`](crate::inflate::inflate) function
/// transitions between these modes as it processes the input stream.
///
/// In the original C implementation (`inflate.h`), modes used arbitrary
/// starting values (e.g. `HEAD=16180`) for memory-corruption detection.
/// In Rust the type system guarantees validity, so standard zero-based
/// discriminants are used instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InflateMode {
    // === Gzip/Zlib Header Processing (wrap != 0) ===
    /// Initial state: determine stream format (zlib / gzip / raw).
    Head,
    /// Gzip: reading flag byte after the magic number.
    Flags,
    /// Gzip: reading 4-byte modification time.
    Time,
    /// Gzip: reading OS byte.
    Os,
    /// Gzip: reading 2-byte extra field length.
    ExLen,
    /// Gzip: reading extra field data.
    Extra,
    /// Gzip: reading null-terminated file name.
    Name,
    /// Gzip: reading null-terminated comment.
    Comment,
    /// Gzip: reading 2-byte header CRC.
    HCrc,
    /// Zlib: reading 4-byte dictionary ID (Adler-32).
    DictId,
    /// Waiting for `inflateSetDictionary()` call.
    Dict,

    // === Block Type Determination ===
    /// Looking for new block type and last-block flag.
    Type,
    /// Same as [`Type`](Self::Type) but skips the header-finish check.
    TypeDo,

    // === Stored (Uncompressed) Block ===
    /// Reading 4-byte stored block header (LEN, NLEN).
    Stored,
    /// Transitional state before [`Copy`](Self::Copy).
    Copy_,
    /// Copying stored block data to output.
    Copy,

    // === Dynamic Huffman Table Building ===
    /// Reading dynamic table descriptor (HLIT, HDIST, HCLEN).
    Table,
    /// Reading code-length code lengths (HCLEN entries).
    LenLens,
    /// Reading literal/length and distance code lengths.
    CodeLens,

    // === Compressed Data Decoding ===
    /// Transitional state before [`Len`](Self::Len).
    Len_,
    /// Decoding literal/length code.
    Len,
    /// Reading length extra bits.
    LenExt,
    /// Decoding distance code.
    Dist,
    /// Reading distance extra bits.
    DistExt,
    /// Copying match from window / already-written output.
    Match,
    /// Outputting a literal byte.
    Lit,

    // === Trailer / Error / Terminal ===
    /// Reading Adler-32 or CRC-32 checksum trailer.
    Check,
    /// Gzip: reading 4-byte original length (ISIZE).
    Length,
    /// Decompression completed successfully, no errors.
    Done,
    /// Fatal error encountered during decompression.
    Bad,
    /// Memory allocation failure.
    Mem,
    /// Searching for a sync point after an error.
    Sync,
}

impl Default for InflateMode {
    /// The default mode is [`Head`](InflateMode::Head), indicating the
    /// beginning of a new stream.
    #[inline]
    fn default() -> Self {
        Self::Head
    }
}

impl fmt::Display for InflateMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::Head => "Head",
            Self::Flags => "Flags",
            Self::Time => "Time",
            Self::Os => "Os",
            Self::ExLen => "ExLen",
            Self::Extra => "Extra",
            Self::Name => "Name",
            Self::Comment => "Comment",
            Self::HCrc => "HCrc",
            Self::DictId => "DictId",
            Self::Dict => "Dict",
            Self::Type => "Type",
            Self::TypeDo => "TypeDo",
            Self::Stored => "Stored",
            Self::Copy_ => "Copy_",
            Self::Copy => "Copy",
            Self::Table => "Table",
            Self::LenLens => "LenLens",
            Self::CodeLens => "CodeLens",
            Self::Len_ => "Len_",
            Self::Len => "Len",
            Self::LenExt => "LenExt",
            Self::Dist => "Dist",
            Self::DistExt => "DistExt",
            Self::Match => "Match",
            Self::Lit => "Lit",
            Self::Check => "Check",
            Self::Length => "Length",
            Self::Done => "Done",
            Self::Bad => "Bad",
            Self::Mem => "Mem",
            Self::Sync => "Sync",
        };
        f.write_str(name)
    }
}

// ---------------------------------------------------------------------------
// InflateState — internal decompression state
// ---------------------------------------------------------------------------

/// Internal state for the DEFLATE decompression engine.
///
/// Contains the sliding window, bit accumulator, Huffman code tables, and all
/// bookkeeping fields needed by the inflate state machine. This struct is the
/// Rust equivalent of C zlib's `inflate_state` from `inflate.h`.
///
/// # Memory Layout
///
/// The struct owns three heap-allocated buffers:
/// - `window` — sliding window for back-references (`Vec<u8>`, lazy allocation)
/// - `codes` — Huffman decode table space (`Vec<Code>`, pre-allocated to
///   [`ENOUGH`] = 1444 entries)
/// - `msg` — optional error message string
///
/// All other fields are inline scalars or fixed-size arrays.
pub struct InflateState {
    // --- Stream format state ---
    /// Current state machine mode.
    pub mode: InflateMode,

    /// `true` if processing the last block in the stream.
    pub last: bool,

    /// Stream wrapper type bitmask.
    ///
    /// - `0` — raw DEFLATE (no header/trailer)
    /// - `1` — zlib format (bit 0 set)
    /// - `2` — gzip format (bit 1 set)
    /// - `3` — auto-detect zlib or gzip (bits 0 and 1 set)
    ///
    /// Bit 2, when set, enables check-value validation.
    pub wrap: i32,

    /// `true` if a preset dictionary has been provided via
    /// `inflateSetDictionary`.
    pub havedict: bool,

    /// Gzip header flags byte, `0` for zlib, `-1` for raw / no header yet.
    pub flags: i32,

    /// Maximum distance limit for `INFLATE_STRICT` mode validation.
    pub dmax: u32,

    /// Running checksum — Adler-32 for zlib, CRC-32 for gzip.
    pub check: u32,

    /// Protected copy of total uncompressed output byte count.
    pub total: u64,

    /// Gzip header storage provided by the caller via `inflate_get_header`.
    /// Replaces the C `gz_headerp` pointer field.
    pub head: Option<Box<GzHeader>>,

    // --- Sliding window ---
    /// Log base-2 of the requested window size (valid range 8..15, or 0 if unset).
    pub wbits: u32,

    /// Actual window size in bytes (`1 << wbits`), or `0` if not yet allocated.
    pub wsize: usize,

    /// Number of valid bytes written to the window so far (saturates at `wsize`).
    pub whave: usize,

    /// Next write position in the circular window buffer.
    pub wnext: usize,

    /// Sliding window buffer for back-references. Initially empty (`Vec::new()`);
    /// allocated to `1 << wbits` bytes on first use during `update_window`.
    pub window: Vec<u8>,

    // --- Bit accumulator ---
    /// Input bit accumulator (holds up to 64 bits of unconsumed input).
    pub hold: u64,

    /// Number of valid bits currently held in [`hold`](Self::hold).
    pub bits: u32,

    // --- Copy / match state ---
    /// Literal byte value, or length of data to copy (stored blocks / matches).
    pub length: u32,

    /// Distance back into output or window for match copy.
    pub offset: u32,

    /// Number of extra bits still needed for the current operation.
    pub extra: u32,

    // --- Huffman decode tables ---
    /// Starting index into [`codes`](Self::codes) for the length/literal table.
    pub lencode_idx: usize,

    /// Starting index into [`codes`](Self::codes) for the distance table.
    pub distcode_idx: usize,

    /// Index bits for the length/literal code table
    /// (first-level table size = `1 << lenbits`).
    pub lenbits: u32,

    /// Index bits for the distance code table
    /// (first-level table size = `1 << distbits`).
    pub distbits: u32,

    // --- Dynamic table building state ---
    /// Number of code-length code lengths (`HCLEN + 4`).
    pub ncode: u32,

    /// Number of literal/length code lengths (`HLIT + 257`).
    pub nlen: u32,

    /// Number of distance code lengths (`HDIST + 1`).
    pub ndist: u32,

    /// Number of code lengths read so far into [`lens`](Self::lens).
    pub have: u32,

    /// Next available space index in [`codes`](Self::codes) during table building.
    pub next: usize,

    /// Temporary storage for code lengths (max 320 entries:
    /// 286 lit/len + 32 dist + 2 padding).
    pub lens: [u16; 320],

    /// Work area for [`inflate_table`](super::tables::inflate_table)
    /// (max 288 entries).
    pub work: [u16; 288],

    /// Space for Huffman decode tables. Pre-allocated to [`ENOUGH`] (1444)
    /// entries: `ENOUGH_LENS` (852) + `ENOUGH_DISTS` (592).
    pub codes: Vec<Code>,

    // --- Error and diagnostic state ---
    /// If `false`, allow invalid distance-too-far-back errors to be ignored
    /// (for fuzzing / testing). Default is `true` (sane mode).
    pub sane: bool,

    /// Bits back of last unprocessed length/literal, used by `inflate_mark`.
    /// `-1` indicates no unprocessed data.
    pub back: i32,

    /// Initial length of the current match, used by `inflate_back`.
    pub was: u32,

    /// Error message associated with [`Bad`](InflateMode::Bad) mode, or `None`.
    pub msg: Option<String>,
}

// ---------------------------------------------------------------------------
// parse_window_bits — decode the overloaded windowBits parameter
// ---------------------------------------------------------------------------

/// Parse the overloaded `windowBits` parameter used by `inflateInit2` and
/// `inflateReset2`.
///
/// The zlib API encodes both the stream format *and* the window size in a
/// single `i32` parameter:
///
/// | Input range      | Format                     | `wrap` | Actual window bits |
/// |------------------|----------------------------|--------|--------------------|
/// | `8..=15`         | zlib (RFC 1950)            | 1      | `wbits`            |
/// | `-15..=-8`       | raw DEFLATE (RFC 1951)     | 0      | `abs(wbits)`       |
/// | `24..=31` (+16)  | gzip only (RFC 1952)       | 2      | `wbits & 15`       |
/// | `40..=47` (+32)  | auto-detect zlib or gzip   | 3      | `wbits & 15`       |
/// | `0`              | keep existing (reset only)  | 1      | 0                  |
///
/// Per the zlib specification, `windowBits = 8` is promoted to `9` because
/// the LZ77 algorithm requires a minimum 512-byte window.
///
/// # Returns
///
/// `Ok((wrap, actual_window_bits))` on success, or
/// `Err(ZlibError::StreamError)` if the parameter is out of range.
///
/// # Examples
///
/// ```ignore
/// let (wrap, wbits) = parse_window_bits(15).unwrap();
/// assert_eq!(wrap, 1);  // zlib format
/// assert_eq!(wbits, 15);
///
/// let (wrap, wbits) = parse_window_bits(-15).unwrap();
/// assert_eq!(wrap, 0);  // raw DEFLATE
/// assert_eq!(wbits, 15);
///
/// let (wrap, wbits) = parse_window_bits(31).unwrap();
/// assert_eq!(wrap, 2);  // gzip
/// assert_eq!(wbits, 15);
///
/// let (wrap, wbits) = parse_window_bits(47).unwrap();
/// assert_eq!(wrap, 3);  // auto-detect
/// assert_eq!(wbits, 15);
/// ```
pub(crate) fn parse_window_bits(wbits: i32) -> Result<(i32, u32), ZlibError> {
    let wrap;
    let mut actual;

    if wbits < 0 {
        // --- Raw DEFLATE: no zlib/gzip wrapper ---
        if wbits < -15 {
            return Err(ZlibError::StreamError);
        }
        wrap = 0;
        actual = (-wbits) as u32;
    } else {
        // --- Zlib, gzip, or auto-detect ---
        // Compute wrap from the high bits of the parameter:
        //   wbits  0..15:  (wbits >> 4) = 0, +1 = 1  (zlib)
        //   wbits 16..31:  (wbits >> 4) = 1, +1 = 2  (gzip)
        //   wbits 32..47:  (wbits >> 4) = 2, +1 = 3  (auto-detect)
        wrap = (wbits >> 4) + 1;

        // Strip format-selection bits to isolate the actual window size.
        // Values >= 48 are left unchanged and will fail validation below.
        if wbits < 48 {
            actual = (wbits & 15) as u32;
        } else {
            actual = wbits as u32;
        }
    }

    // Validate the window size.
    // `0` is permitted (means "keep existing window" when used via `reset2`).
    // Valid window sizes are 8 through 15.
    if actual != 0 && !(8..=15).contains(&actual) {
        return Err(ZlibError::StreamError);
    }

    // Per zlib specification, windowBits = 8 is promoted to 9.
    // The minimum practical window for the LZ77 algorithm is 512 bytes.
    if actual == 8 {
        actual = 9;
    }

    Ok((wrap, actual))
}

// ---------------------------------------------------------------------------
// InflateState implementation
// ---------------------------------------------------------------------------

impl InflateState {
    /// Create a new `InflateState` configured for the given window bits.
    ///
    /// This is the Rust equivalent of the allocation + initialization portion
    /// of C zlib's `inflateInit2_` / `inflateReset2`.
    ///
    /// # Arguments
    ///
    /// * `wbits_param` — Overloaded window-bits parameter (see
    ///   [`parse_window_bits`] for encoding details).
    ///
    /// # Errors
    ///
    /// Returns [`ZlibError::StreamError`] if `wbits_param` is out of the
    /// valid range.
    pub fn new(wbits_param: i32) -> Result<Self, ZlibError> {
        let (wrap, actual_wbits) = parse_window_bits(wbits_param)?;

        Ok(Self {
            mode: InflateMode::Head,
            last: false,
            wrap,
            havedict: false,
            flags: if wrap == 0 { -1 } else { 0 },
            dmax: 32768,
            check: 0,
            total: 0,
            head: None,

            wbits: actual_wbits,
            wsize: 0,
            whave: 0,
            wnext: 0,
            window: Vec::new(),

            hold: 0,
            bits: 0,

            length: 0,
            offset: 0,
            extra: 0,

            lencode_idx: 0,
            distcode_idx: 0,
            lenbits: 0,
            distbits: 0,

            ncode: 0,
            nlen: 0,
            ndist: 0,
            have: 0,
            next: 0,
            lens: [0u16; 320],
            work: [0u16; 288],
            codes: vec![Code::default(); ENOUGH],

            sane: true,
            back: -1,
            was: 0,
            msg: None,
        })
    }

    /// Reset state for a new decompression stream, preserving the existing
    /// window allocation.
    ///
    /// This is the Rust equivalent of `inflateResetKeep` + `inflateReset` in
    /// C zlib's `inflate.c`. The sliding window buffer is **not** deallocated
    /// and the `wbits` / `wsize` / `whave` / `wnext` fields are **not** cleared,
    /// allowing the window to be reused for the next stream.
    pub fn reset(&mut self) {
        self.mode = InflateMode::Head;
        self.last = false;
        self.havedict = false;
        self.flags = if self.wrap == 0 { -1 } else { 0 };
        self.dmax = 32768;
        self.check = 0;
        self.total = 0;

        self.hold = 0;
        self.bits = 0;

        self.length = 0;
        self.offset = 0;
        self.extra = 0;

        self.lencode_idx = 0;
        self.distcode_idx = 0;
        self.lenbits = 0;
        self.distbits = 0;

        self.ncode = 0;
        self.nlen = 0;
        self.ndist = 0;
        self.have = 0;
        self.next = 0;

        self.sane = true;
        self.back = -1;
        self.was = 0;
        self.msg = None;
        // Note: window, wbits, wsize, whave, wnext are intentionally preserved.
    }

    /// Reset state with a potentially different window size.
    ///
    /// This is the Rust equivalent of `inflateReset2` in C zlib's `inflate.c`.
    /// If the new window size differs from the current one, the window buffer
    /// is deallocated so that it will be re-allocated on first use.
    ///
    /// # Arguments
    ///
    /// * `wbits_param` — Overloaded window-bits parameter (see
    ///   [`parse_window_bits`]). Pass `0` to keep the existing window size.
    ///
    /// # Errors
    ///
    /// Returns [`ZlibError::StreamError`] if `wbits_param` is out of range.
    pub fn reset2(&mut self, wbits_param: i32) -> Result<(), ZlibError> {
        let (wrap, actual_wbits) = parse_window_bits(wbits_param)?;

        // If the window size changed, deallocate the old window so it will be
        // freshly allocated on next use.
        if actual_wbits != 0 && actual_wbits != self.wbits {
            self.window = Vec::new();
            self.wsize = 0;
            self.whave = 0;
            self.wnext = 0;
        }

        self.wrap = wrap;
        if actual_wbits != 0 {
            self.wbits = actual_wbits;
        }
        self.reset();
        Ok(())
    }

    /// Look up a length/literal code entry from the Huffman decode table.
    ///
    /// `index` is typically `(hold & ((1 << lenbits) - 1))` — the low
    /// `lenbits` bits of the bit accumulator.
    #[inline(always)]
    pub fn len_code(&self, index: usize) -> Code {
        self.codes[self.lencode_idx + index]
    }

    /// Look up a distance code entry from the Huffman decode table.
    ///
    /// `index` is typically `(hold & ((1 << distbits) - 1))` — the low
    /// `distbits` bits of the bit accumulator.
    #[inline(always)]
    pub fn dist_code(&self, index: usize) -> Code {
        self.codes[self.distcode_idx + index]
    }

    /// Configure the state to use the pre-built **fixed** Huffman tables for
    /// DEFLATE block type 1.
    ///
    /// Copies the static [`LENFIX`] (512 entries) and [`DISTFIX`] (32 entries)
    /// arrays into the beginning of the [`codes`](Self::codes) buffer and sets
    /// the decode-table indices and bit widths accordingly.
    ///
    /// This replaces the C pattern of assigning `state->lencode = lenfix;` /
    /// `state->distcode = distfix;` with a safe, owned-buffer approach.
    pub fn use_fixed_codes(&mut self) {
        // Copy the fixed length/literal table into codes[0..512].
        self.codes[..LENFIX.len()].copy_from_slice(&LENFIX);

        // Copy the fixed distance table into codes[512..544].
        let dist_start = LENFIX.len();
        self.codes[dist_start..dist_start + DISTFIX.len()].copy_from_slice(&DISTFIX);

        // Point decode indices at the copied tables.
        self.lencode_idx = 0;
        self.distcode_idx = dist_start;

        // Set the decode-table bit widths for fixed Huffman codes.
        self.lenbits = FIXLCODES_BITS; // 9
        self.distbits = FIXDCODES_BITS; // 5
    }
}

// ---------------------------------------------------------------------------
// Clone implementation (for inflate_copy)
// ---------------------------------------------------------------------------

impl Clone for InflateState {
    /// Create a deep clone of the inflate state.
    ///
    /// The `head` field is set to `None` in the clone because, like C zlib's
    /// `inflateCopy`, the gzip-header reference is not transferred — the
    /// caller must re-register a header via `inflate_get_header` on the clone.
    fn clone(&self) -> Self {
        Self {
            mode: self.mode,
            last: self.last,
            wrap: self.wrap,
            havedict: self.havedict,
            flags: self.flags,
            dmax: self.dmax,
            check: self.check,
            total: self.total,
            // Per C zlib inflateCopy: gz_header pointer is NOT transferred.
            head: None,

            wbits: self.wbits,
            wsize: self.wsize,
            whave: self.whave,
            wnext: self.wnext,
            window: self.window.clone(),

            hold: self.hold,
            bits: self.bits,

            length: self.length,
            offset: self.offset,
            extra: self.extra,

            lencode_idx: self.lencode_idx,
            distcode_idx: self.distcode_idx,
            lenbits: self.lenbits,
            distbits: self.distbits,

            ncode: self.ncode,
            nlen: self.nlen,
            ndist: self.ndist,
            have: self.have,
            next: self.next,
            lens: self.lens,
            work: self.work,
            codes: self.codes.clone(),

            sane: self.sane,
            back: self.back,
            was: self.was,
            msg: self.msg.clone(),
        }
    }
}

// ---------------------------------------------------------------------------
// Debug implementation
// ---------------------------------------------------------------------------

impl fmt::Debug for InflateState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InflateState")
            .field("mode", &self.mode)
            .field("last", &self.last)
            .field("wrap", &self.wrap)
            .field("havedict", &self.havedict)
            .field("flags", &self.flags)
            .field("dmax", &self.dmax)
            .field("check", &self.check)
            .field("total", &self.total)
            .field("head.is_some", &self.head.is_some())
            .field("wbits", &self.wbits)
            .field("wsize", &self.wsize)
            .field("whave", &self.whave)
            .field("wnext", &self.wnext)
            .field("window.len", &self.window.len())
            .field("hold", &self.hold)
            .field("bits", &self.bits)
            .field("length", &self.length)
            .field("offset", &self.offset)
            .field("extra", &self.extra)
            .field("lencode_idx", &self.lencode_idx)
            .field("distcode_idx", &self.distcode_idx)
            .field("lenbits", &self.lenbits)
            .field("distbits", &self.distbits)
            .field("ncode", &self.ncode)
            .field("nlen", &self.nlen)
            .field("ndist", &self.ndist)
            .field("have", &self.have)
            .field("next", &self.next)
            .field("codes.len", &self.codes.len())
            .field("sane", &self.sane)
            .field("back", &self.back)
            .field("was", &self.was)
            .field("msg", &self.msg)
            .finish()
    }
}
