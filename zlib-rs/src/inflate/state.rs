//! Inflate decompression state definitions and mode enum.
//!
//! This module provides the core types for the inflate (decompression) engine:
//!
//! - [`InflateMode`] — A 32-variant enum encoding the state machine for DEFLATE
//!   decompression, replacing the C `inflate_mode` enum from `inflate.h` lines
//!   20–53 with compile-time exhaustiveness checking.
//!
//! - [`InflateState`] — The full decompression state holding approximately 7 KB
//!   of data (not including the sliding window), ported from the C
//!   `struct inflate_state` in `inflate.h` lines 82–126.
//!
//! - [`CodeTableRef`] — A safe reference to either a fixed (compile-time const)
//!   or dynamic (runtime-built) Huffman code table, replacing the raw `code *`
//!   pointers in the C implementation.
//!
//! # Safety
//!
//! This module contains zero `unsafe` code. All state transitions and buffer
//! accesses use safe Rust types with bounds checking.

use super::table::{Code, ENOUGH};
use crate::stream::GzHeader;

// ─── InflateMode ──────────────────────────────────────────────────────────────

/// Inflate decompression state machine modes.
///
/// These 32 modes represent the current processing phase of the inflate engine.
/// Each variant corresponds to a state in the C `inflate_mode` enum defined in
/// `inflate.h` lines 20–53, with discriminant values starting at 16180.
///
/// # State Transition Diagram
///
/// Most modes can transition to [`Bad`](InflateMode::Bad) or
/// [`Mem`](InflateMode::Mem) on error (not shown for clarity).
///
/// ```text
/// Process header:
///     HEAD → (gzip) or (zlib) or (raw)
///     (gzip) → FLAGS → TIME → OS → EXLEN → EXTRA → NAME → COMMENT → HCRC → TYPE
///     (zlib) → DICTID or TYPE
///     DICTID → DICT → TYPE
///     (raw) → TYPEDO
/// Read deflate blocks:
///     TYPE → TYPEDO → STORED or TABLE or LEN_ or CHECK
///     STORED → COPY_ → COPY → TYPE
///     TABLE → LENLENS → CODELENS → LEN_
///     LEN_ → LEN
/// Read deflate codes in fixed or dynamic block:
///     LEN → LENEXT or LIT or TYPE
///     LENEXT → DIST → DISTEXT → MATCH → LEN
///     LIT → LEN
/// Process trailer:
///     CHECK → LENGTH → DONE
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum InflateMode {
    /// Waiting for magic header. (i)
    Head = 16180,
    /// Waiting for method and flags — gzip. (i)
    Flags,
    /// Waiting for modification time — gzip. (i)
    Time,
    /// Waiting for extra flags and operating system — gzip. (i)
    Os,
    /// Waiting for extra length — gzip. (i)
    ExLen,
    /// Waiting for extra bytes — gzip. (i)
    Extra,
    /// Waiting for end of file name — gzip. (i)
    Name,
    /// Waiting for end of comment — gzip. (i)
    Comment,
    /// Waiting for header crc — gzip. (i)
    Hcrc,
    /// Waiting for dictionary check value. (i)
    DictId,
    /// Waiting for `inflate_set_dictionary()` call.
    Dict,
    /// Waiting for type bits, including last-flag bit. (i)
    Type,
    /// Same as [`Type`](Self::Type), but skip check to exit inflate on new block. (i)
    TypeDo,
    /// Waiting for stored size — length and complement. (i)
    Stored,
    /// Same as [`Copy`](Self::Copy) below, but only first time in. (i/o)
    Copy_,
    /// Waiting for input or output to copy stored block. (i/o)
    Copy,
    /// Waiting for dynamic block table lengths. (i)
    Table,
    /// Waiting for code length code lengths. (i)
    LenLens,
    /// Waiting for length/lit and distance code lengths. (i)
    CodeLens,
    /// Same as [`Len`](Self::Len) below, but only first time in. (i)
    Len_,
    /// Waiting for length/lit/eob code. (i)
    Len,
    /// Waiting for length extra bits. (i)
    LenExt,
    /// Waiting for distance code. (i)
    Dist,
    /// Waiting for distance extra bits. (i)
    DistExt,
    /// Waiting for output space to copy string. (o)
    Match,
    /// Waiting for output space to write literal. (o)
    Lit,
    /// Waiting for 32-bit check value. (i)
    Check,
    /// Waiting for 32-bit length — gzip. (i)
    Length,
    /// Finished check, done — remain here until reset.
    Done,
    /// Got a data error — remain here until reset.
    Bad,
    /// Got an `inflate()` memory error — remain here until reset.
    Mem,
    /// Looking for synchronization bytes to restart `inflate()`.
    Sync,
}

impl InflateMode {
    /// Returns `true` if this mode indicates an error condition.
    ///
    /// Error modes are [`Bad`](Self::Bad) (data error) and
    /// [`Mem`](Self::Mem) (memory allocation failure).
    #[inline]
    #[must_use]
    pub fn is_error(self) -> bool {
        matches!(self, Self::Bad | Self::Mem)
    }

    /// Returns `true` if this is a terminal mode.
    ///
    /// Terminal modes are [`Done`](Self::Done) (successful completion),
    /// [`Bad`](Self::Bad) (data error), and [`Mem`](Self::Mem) (memory error).
    /// The state machine remains in these modes until explicitly reset.
    #[inline]
    #[must_use]
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Done | Self::Bad | Self::Mem)
    }
}

// Compile-time verification of discriminant range.
const _: () = assert!(InflateMode::Head as u32 == 16180);
const _: () = assert!(InflateMode::Sync as u32 == 16211);

// ─── CodeTableRef ─────────────────────────────────────────────────────────────

/// Reference to a Huffman code table — either fixed (const) or dynamic (owned).
///
/// In the C zlib implementation, `lencode` and `distcode` are raw `code *`
/// pointers that point either to a static const table (`lenfix`/`distfix` from
/// `inffixed.h`) or into the `codes[]` array within `inflate_state`. This Rust
/// enum replaces those raw pointers with safe alternatives:
///
/// - [`Fixed`](Self::Fixed) holds a `&'static [Code]` slice referencing
///   compile-time const tables (e.g., `LENFIX[512]` or `DISTFIX[32]`).
/// - [`Dynamic`](Self::Dynamic) holds a `usize` start index into the
///   `InflateState::codes` vector for tables built at runtime during
///   dynamic block decoding.
#[derive(Debug, Clone, Copy)]
pub enum CodeTableRef {
    /// Reference to a pre-computed fixed Huffman table from `inffixed.h`.
    ///
    /// The contained slice is a `&'static [Code]` pointing to one of the
    /// compile-time const tables (`LENFIX` or `DISTFIX`).
    Fixed(&'static [Code]),

    /// Index into `InflateState::codes` for a dynamically built table.
    ///
    /// The `usize` value is the start offset in the codes vector where
    /// the table begins. Code lookups add the masked hold-register bits
    /// to this base index.
    Dynamic(usize),
}

// ─── InflateState ─────────────────────────────────────────────────────────────

/// Internal state for the inflate decompression engine.
///
/// Maintains approximately 7 KB of data, not including the allocated sliding
/// window which is up to 32 KB. This struct owns all its buffers via Rust's
/// [`Vec`] type, replacing the manually managed pointers in the C
/// `struct inflate_state` from `inflate.h` lines 82–126.
///
/// The C back-pointer `z_streamp strm` is omitted — in Rust, the state is
/// passed alongside the stream rather than holding a raw pointer back to it.
///
/// # Cloning
///
/// `InflateState` implements [`Clone`] for deep copies, which is required by
/// `inflateCopy()` to duplicate a decompression session. All owned data
/// including the sliding window and code tables are fully cloned.
#[derive(Debug, Clone)]
pub struct InflateState {
    // ── State machine ──────────────────────────────────────────────────────
    /// Current inflate mode — determines which state machine branch executes.
    pub(crate) mode: InflateMode,

    /// `true` if processing the last block in the stream.
    pub(crate) last: bool,

    /// Wrapping mode: bit 0 = zlib, bit 1 = gzip, bit 2 = validate checksum.
    ///
    /// Controls header/trailer processing and checksum computation.
    pub(crate) wrap: i32,

    /// `true` if a preset dictionary has been provided via
    /// `inflate_set_dictionary()`.
    pub(crate) havedict: bool,

    /// Gzip header method and flags (0 if zlib, −1 if raw or no header yet).
    pub(crate) flags: i32,

    /// Maximum backward distance for `INFLATE_STRICT` mode.
    ///
    /// Defaults to 32768 (the maximum window size).
    pub(crate) dmax: u32,

    /// Protected copy of the running checksum value.
    ///
    /// Contains either Adler-32 (zlib format) or CRC-32 (gzip format)
    /// depending on the stream wrapper.
    pub(crate) check: u64,

    /// Protected copy of the total uncompressed output byte count.
    pub(crate) total: u64,

    /// Where to save gzip header information during decompression.
    ///
    /// `None` if no header storage was requested via `inflate_get_header()`.
    pub(crate) head: Option<Box<GzHeader>>,

    // ── Sliding window ─────────────────────────────────────────────────────
    /// Log base 2 of the requested window size (8..=15).
    pub(crate) wbits: u32,

    /// Window size in bytes, or zero if not yet using the window.
    pub(crate) wsize: u32,

    /// Number of valid bytes currently in the window.
    pub(crate) whave: u32,

    /// Current write position (index) in the circular window buffer.
    pub(crate) wnext: u32,

    /// Allocated sliding window buffer.
    ///
    /// Replaces `unsigned char FAR *window` from the C implementation with
    /// an owned vector that is automatically freed on drop.
    pub(crate) window: Vec<u8>,

    // ── Bit accumulator ────────────────────────────────────────────────────
    /// Input bit accumulator — bits are shifted in from the low end.
    pub(crate) hold: u64,

    /// Number of valid bits currently in [`hold`](Self::hold).
    pub(crate) bits: u32,

    // ── String and stored block copying ────────────────────────────────────
    /// Literal byte value or remaining length of data to copy.
    pub(crate) length: u32,

    /// Backward distance for string copy operations.
    pub(crate) offset: u32,

    // ── Table and code decoding ────────────────────────────────────────────
    /// Number of extra bits needed for the current code.
    pub(crate) extra: u32,

    // ── Fixed and dynamic code tables ──────────────────────────────────────
    /// Reference to the length/literal code table.
    ///
    /// Points either to the fixed `LENFIX` table or into [`codes`](Self::codes).
    pub(crate) lencode: CodeTableRef,

    /// Reference to the distance code table.
    ///
    /// Points either to the fixed `DISTFIX` table or into [`codes`](Self::codes).
    pub(crate) distcode: CodeTableRef,

    /// Root table index bits for length/literal codes.
    pub(crate) lenbits: u32,

    /// Root table index bits for distance codes.
    pub(crate) distbits: u32,

    // ── Dynamic table building ─────────────────────────────────────────────
    /// Number of code length code lengths read so far.
    pub(crate) ncode: u32,

    /// Number of length code lengths in the current dynamic header.
    pub(crate) nlen: u32,

    /// Number of distance code lengths in the current dynamic header.
    pub(crate) ndist: u32,

    /// Number of code lengths accumulated in [`lens`](Self::lens).
    pub(crate) have: u32,

    /// Next available write position (index) in [`codes`](Self::codes).
    pub(crate) next: usize,

    /// Temporary storage for code lengths during dynamic table construction.
    ///
    /// Sized at 320 to hold up to 286 length/literal + 30 distance code
    /// lengths, plus 4 spare entries (matching the C `unsigned short lens[320]`).
    pub(crate) lens: [u16; 320],

    /// Work area for code table building in `inflate_table()`.
    ///
    /// Sized at 288 to accommodate up to 286 length/literal symbols plus
    /// 2 spare entries (matching the C `unsigned short work[288]`).
    pub(crate) work: [u16; 288],

    /// Pre-allocated space for Huffman code tables.
    ///
    /// Sized to [`ENOUGH`] (1444) entries — the maximum combined size of
    /// the length/literal table (852) and the distance table (592).
    pub(crate) codes: Vec<Code>,

    // ── Safety and progress tracking ───────────────────────────────────────
    /// If `false`, allow invalid "distance too far back" errors to be
    /// tolerated (for testing purposes via `inflate_undermine()`).
    pub(crate) sane: bool,

    /// Bits back of the last unprocessed length/literal code.
    ///
    /// Used by `inflate_mark()` to report decompression progress.
    /// A value of −1 indicates no unprocessed code.
    pub(crate) back: i32,

    /// Initial length of the current match (for progress reporting).
    pub(crate) was: u32,
}

// ─── InflateState implementation ──────────────────────────────────────────────

impl InflateState {
    /// Creates a new `InflateState` with default initial values.
    ///
    /// The state is initialized to begin processing from the stream header
    /// ([`InflateMode::Head`]). The codes table is pre-allocated to
    /// [`ENOUGH`] entries, and all other fields are set to their initial
    /// defaults matching the C `inflateResetKeep()` initialization.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Look up a length/literal code from the current length code table.
    ///
    /// The `index` is typically the low [`lenbits`](Self::lenbits) bits of the
    /// hold register, masked by `(1 << lenbits) - 1`.
    ///
    /// # Panics
    ///
    /// Panics if `index` exceeds the bounds of the referenced code table.
    /// This cannot occur with valid DEFLATE streams.
    #[inline]
    #[must_use]
    pub(crate) fn len_code(&self, index: u32) -> Code {
        match self.lencode {
            CodeTableRef::Fixed(table) => table[index as usize],
            CodeTableRef::Dynamic(start) => self.codes[start + index as usize],
        }
    }

    /// Look up a distance code from the current distance code table.
    ///
    /// The `index` is typically the low [`distbits`](Self::distbits) bits of
    /// the hold register, masked by `(1 << distbits) - 1`.
    ///
    /// # Panics
    ///
    /// Panics if `index` exceeds the bounds of the referenced code table.
    /// This cannot occur with valid DEFLATE streams.
    #[inline]
    #[must_use]
    pub(crate) fn dist_code(&self, index: u32) -> Code {
        match self.distcode {
            CodeTableRef::Fixed(table) => table[index as usize],
            CodeTableRef::Dynamic(start) => self.codes[start + index as usize],
        }
    }

    /// Returns a shared reference to the stored gzip header, if one was
    /// registered via [`inflate_get_header`](super::inflate_get_header).
    ///
    /// This method is the Rust-side equivalent of inspecting the `gz_header *`
    /// pointer that the C API's `inflateGetHeader()` stores.  After
    /// decompression completes (or after the gzip header has been fully
    /// parsed), callers can inspect the header fields through this reference.
    ///
    /// # Returns
    ///
    /// `Some(&GzHeader)` if a header was registered, `None` otherwise.
    #[must_use]
    pub fn header(&self) -> Option<&GzHeader> {
        self.head.as_deref()
    }

    /// Returns a mutable reference to the stored gzip header, if one was
    /// registered via [`inflate_get_header`](super::inflate_get_header).
    ///
    /// Allows callers to modify header fields before or after decompression.
    ///
    /// # Returns
    ///
    /// `Some(&mut GzHeader)` if a header was registered, `None` otherwise.
    #[must_use]
    pub fn header_mut(&mut self) -> Option<&mut GzHeader> {
        self.head.as_deref_mut()
    }

    /// Ensure the sliding window buffer is allocated.
    ///
    /// If the window is empty, allocates a buffer of `2^wbits` bytes filled
    /// with zeroes. Returns `true` on success, or `false` if
    /// [`wbits`](Self::wbits) is invalid (would produce a zero-size or
    /// overflowing window).
    #[must_use]
    pub(crate) fn ensure_window(&mut self) -> bool {
        if self.window.is_empty() {
            let size = match 1usize.checked_shl(self.wbits) {
                Some(s) if s > 0 => s,
                _ => return false,
            };
            self.window.resize(size, 0);
        }
        true
    }

    /// Write data into the circular sliding window buffer.
    ///
    /// Copies the contents of `data` into the window starting at the current
    /// write position ([`wnext`](Self::wnext)), wrapping around as needed.
    /// Updates [`wnext`](Self::wnext) and [`whave`](Self::whave) to reflect
    /// the new state. If `data` is larger than the window, only the last
    /// [`wsize`](Self::wsize) bytes are retained.
    ///
    /// The window must be allocated before calling this method. If
    /// [`wsize`](Self::wsize) is zero, it is initialized to `2^wbits`.
    // Retained for inflate_back window management; used by the callback
    // decompression path for direct-to-window output.
    #[allow(dead_code, clippy::cast_possible_truncation)] // dist and remaining bounded by wsize (≤ 32768)
    pub(crate) fn window_write(&mut self, data: &[u8]) {
        // Initialize window size on first use.
        if self.wsize == 0 {
            self.wsize = 1u32.wrapping_shl(self.wbits);
            self.wnext = 0;
            self.whave = 0;
        }

        let wsize = self.wsize as usize;
        let copy_len = data.len();

        if copy_len >= wsize {
            // Data fills or exceeds the entire window — keep the last wsize bytes.
            let start = copy_len - wsize;
            self.window[..wsize].copy_from_slice(&data[start..]);
            self.wnext = 0;
            self.whave = self.wsize;
        } else {
            // Partial write into circular buffer.
            let wnext = self.wnext as usize;
            let dist = (wsize - wnext).min(copy_len);

            // Copy first portion (from wnext to end of window or end of data).
            self.window[wnext..wnext + dist].copy_from_slice(&data[..dist]);

            let remaining = copy_len - dist;
            if remaining > 0 {
                // Wrapped around: copy remainder at the start of the window.
                self.window[..remaining].copy_from_slice(&data[dist..]);
                self.wnext = remaining as u32;
                self.whave = self.wsize;
            } else {
                self.wnext += dist as u32;
                if self.wnext == self.wsize {
                    self.wnext = 0;
                }
                if self.whave < self.wsize {
                    self.whave += dist as u32;
                }
            }
        }
    }
}

// ─── Default ──────────────────────────────────────────────────────────────────

impl Default for InflateState {
    fn default() -> Self {
        Self {
            mode: InflateMode::Head,
            last: false,
            wrap: 0,
            havedict: false,
            flags: -1,
            dmax: 32_768,
            check: 0,
            total: 0,
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
            lencode: CodeTableRef::Dynamic(0),
            distcode: CodeTableRef::Dynamic(0),
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
        }
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inflate::table::Code;

    // ── InflateMode tests ──────────────────────────────────────────────

    #[test]
    fn mode_head_discriminant() {
        assert_eq!(InflateMode::Head as u32, 16180);
    }

    #[test]
    fn mode_sync_discriminant() {
        assert_eq!(InflateMode::Sync as u32, 16211);
    }

    #[test]
    fn mode_all_32_sequential() {
        let modes = [
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
            InflateMode::Copy_,
            InflateMode::Copy,
            InflateMode::Table,
            InflateMode::LenLens,
            InflateMode::CodeLens,
            InflateMode::Len_,
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
        assert_eq!(modes.len(), 32);
        #[allow(clippy::cast_possible_truncation)]
        for (i, mode) in modes.iter().enumerate() {
            assert_eq!(
                *mode as u32,
                16180 + i as u32,
                "Mode {mode:?} has wrong discriminant"
            );
        }
    }

    #[test]
    fn mode_is_error_true() {
        assert!(InflateMode::Bad.is_error());
        assert!(InflateMode::Mem.is_error());
    }

    #[test]
    fn mode_is_error_false() {
        assert!(!InflateMode::Done.is_error());
        assert!(!InflateMode::Head.is_error());
        assert!(!InflateMode::Sync.is_error());
    }

    #[test]
    fn mode_is_terminal_true() {
        assert!(InflateMode::Done.is_terminal());
        assert!(InflateMode::Bad.is_terminal());
        assert!(InflateMode::Mem.is_terminal());
    }

    #[test]
    fn mode_is_terminal_false() {
        assert!(!InflateMode::Head.is_terminal());
        assert!(!InflateMode::Sync.is_terminal());
        assert!(!InflateMode::Check.is_terminal());
    }

    #[test]
    fn mode_copy_clone_eq() {
        let m1 = InflateMode::Len;
        let m2 = m1;
        assert_eq!(m1, m2);
    }

    #[test]
    fn mode_debug_format() {
        assert_eq!(format!("{:?}", InflateMode::Head), "Head");
        assert_eq!(format!("{:?}", InflateMode::Sync), "Sync");
    }

    // ── InflateState default values ────────────────────────────────────

    #[test]
    fn state_new_defaults() {
        let s = InflateState::new();
        assert_eq!(s.mode, InflateMode::Head);
        assert!(!s.last);
        assert_eq!(s.wrap, 0);
        assert!(!s.havedict);
        assert_eq!(s.flags, -1);
        assert_eq!(s.dmax, 32_768);
        assert_eq!(s.check, 0);
        assert_eq!(s.total, 0);
        assert!(s.head.is_none());
        assert_eq!(s.wbits, 0);
        assert_eq!(s.wsize, 0);
        assert_eq!(s.whave, 0);
        assert_eq!(s.wnext, 0);
        assert!(s.window.is_empty());
        assert_eq!(s.hold, 0);
        assert_eq!(s.bits, 0);
        assert_eq!(s.length, 0);
        assert_eq!(s.offset, 0);
        assert_eq!(s.extra, 0);
        assert_eq!(s.lenbits, 0);
        assert_eq!(s.distbits, 0);
        assert_eq!(s.ncode, 0);
        assert_eq!(s.nlen, 0);
        assert_eq!(s.ndist, 0);
        assert_eq!(s.have, 0);
        assert_eq!(s.next, 0);
        assert!(s.sane);
        assert_eq!(s.back, -1);
        assert_eq!(s.was, 0);
    }

    #[test]
    fn state_codes_capacity() {
        let s = InflateState::new();
        assert_eq!(s.codes.len(), ENOUGH);
        assert_eq!(s.codes.len(), 1444);
    }

    #[test]
    fn state_array_sizes() {
        let s = InflateState::new();
        assert_eq!(s.lens.len(), 320);
        assert_eq!(s.work.len(), 288);
    }

    #[test]
    fn state_default_matches_new() {
        let s1 = InflateState::new();
        let s2 = InflateState::default();
        assert_eq!(s1.mode, s2.mode);
        assert_eq!(s1.flags, s2.flags);
        assert_eq!(s1.dmax, s2.dmax);
        assert_eq!(s1.codes.len(), s2.codes.len());
    }

    #[test]
    fn state_clone_deep() {
        let mut s1 = InflateState::new();
        s1.mode = InflateMode::Len;
        s1.wbits = 15;
        s1.hold = 0xDEAD_BEEF;
        let s2 = s1.clone();
        assert_eq!(s2.mode, InflateMode::Len);
        assert_eq!(s2.wbits, 15);
        assert_eq!(s2.hold, 0xDEAD_BEEF);
    }

    // ── ensure_window tests ────────────────────────────────────────────

    #[test]
    fn ensure_window_allocates() {
        let mut s = InflateState::new();
        s.wbits = 15;
        assert!(s.ensure_window());
        assert_eq!(s.window.len(), 32768);
    }

    #[test]
    fn ensure_window_idempotent() {
        let mut s = InflateState::new();
        s.wbits = 10;
        assert!(s.ensure_window());
        let len = s.window.len();
        assert!(s.ensure_window());
        assert_eq!(s.window.len(), len);
    }

    #[test]
    fn ensure_window_various_sizes() {
        for wbits in 8u32..=15 {
            let mut s = InflateState::new();
            s.wbits = wbits;
            assert!(s.ensure_window());
            assert_eq!(s.window.len(), 1 << wbits);
        }
    }

    // ── window_write tests ─────────────────────────────────────────────

    #[test]
    fn window_write_small() {
        let mut s = InflateState::new();
        s.wbits = 8;
        let _ = s.ensure_window();
        s.window_write(&[1, 2, 3, 4, 5]);
        assert_eq!(s.wnext, 5);
        assert_eq!(s.whave, 5);
        assert_eq!(&s.window[..5], &[1, 2, 3, 4, 5]);
    }

    #[test]
    fn window_write_exact_fill() {
        let mut s = InflateState::new();
        s.wbits = 8;
        let _ = s.ensure_window();
        #[allow(clippy::cast_possible_truncation)]
        let data: Vec<u8> = (0u16..256).map(|i| i as u8).collect();
        s.window_write(&data);
        assert_eq!(s.wnext, 0);
        assert_eq!(s.whave, 256);
    }

    #[test]
    fn window_write_overflow() {
        let mut s = InflateState::new();
        s.wbits = 8;
        let _ = s.ensure_window();
        let data: Vec<u8> = (0u16..300).map(|i| (i & 0xFF) as u8).collect();
        s.window_write(&data);
        assert_eq!(s.wnext, 0);
        assert_eq!(s.whave, 256);
        assert_eq!(s.window[0], 44);
    }

    // ── Code table lookup tests ────────────────────────────────────────

    #[test]
    fn len_code_dynamic_lookup() {
        let mut s = InflateState::new();
        s.codes[0] = Code::new(0, 7, 256);
        s.codes[2] = Code::new(0, 8, 32);
        s.lencode = CodeTableRef::Dynamic(0);
        assert_eq!(s.len_code(0).val, 256);
        assert_eq!(s.len_code(2).val, 32);
    }

    #[test]
    fn dist_code_dynamic_lookup() {
        let mut s = InflateState::new();
        s.codes[10] = Code::new(16, 5, 1);
        s.distcode = CodeTableRef::Dynamic(10);
        assert_eq!(s.dist_code(0).op, 16);
        assert_eq!(s.dist_code(0).val, 1);
    }

    #[test]
    fn code_table_ref_debug() {
        let r = CodeTableRef::Dynamic(42);
        assert!(format!("{r:?}").contains("42"));
    }
}
