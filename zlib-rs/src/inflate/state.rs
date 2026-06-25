//! Decompressor state and mode machine (`inflate.h`).
//!
//! Safe-Rust port of the C `inflate_state` struct and its `inflate_mode` enum.
//! This is the foundation *data model*: it owns the sliding window, the bit
//! accumulator scalars, the dynamic-table scratch space, and the decode tables.
//! The decoding *algorithm* (the `switch (state->mode)` driver loop with its
//! `goto inf_leave` exits) is not part of this milestone and is implemented on
//! top of this state in a subsequent milestone; this file contains **no**
//! decode logic.
//!
//! Ownership is inverted relative to C: the C `inflate_state` holds a `strm`
//! back-pointer to its `z_stream`, whereas here [`crate::stream::ZStream`] owns
//! the boxed [`InflateState`], so no back-pointer is stored.

use alloc::vec::Vec;

use crate::gz_header::GzHeader;
use crate::inflate::tables::{Code, ENOUGH};

/// The inflate state machine — a faithful, exhaustive port of the C
/// `inflate_mode` enum (`inflate.h`).
///
/// The first state deliberately starts at the large sentinel value `16180`
/// (matching C `HEAD = 16180`); the unusual base makes an uninitialized or
/// corrupted mode value easy to detect. The remaining variants follow in the
/// exact C order so their discriminants line up one-for-one with the C enum.
///
/// Variant names are idiomatic Rust `CamelCase`; the two C "first time in"
/// states `COPY_` and `LEN_` map to [`InflateMode::CopyBegin`] and
/// [`InflateMode::LenBegin`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum InflateMode {
    /// `HEAD` — waiting for the magic header byte(s).
    Head = 16180,
    /// `FLAGS` — waiting for the gzip method and flag bytes.
    Flags,
    /// `TIME` — waiting for the gzip modification time.
    Time,
    /// `OS` — waiting for the gzip extra-flags and OS bytes.
    Os,
    /// `EXLEN` — waiting for the gzip "extra" field length.
    Exlen,
    /// `EXTRA` — copying the gzip "extra" field bytes.
    Extra,
    /// `NAME` — reading the gzip original file name.
    Name,
    /// `COMMENT` — reading the gzip comment.
    Comment,
    /// `HCRC` — waiting for the gzip header CRC.
    Hcrc,
    /// `DICTID` — waiting for the zlib dictionary check value.
    Dictid,
    /// `DICT` — waiting for an `inflateSetDictionary` call.
    Dict,
    /// `TYPE` — waiting for the block type bits (including the last-block flag).
    Type,
    /// `TYPEDO` — as `TYPE`, but skips the new-block exit check.
    Typedo,
    /// `STORED` — waiting for a stored block's length and its complement.
    Stored,
    /// `COPY_` — first entry into [`InflateMode::Copy`].
    CopyBegin,
    /// `COPY` — copying a stored block, waiting for input or output space.
    Copy,
    /// `TABLE` — waiting for the dynamic-block table lengths.
    Table,
    /// `LENLENS` — waiting for the code-length code lengths.
    Lenlens,
    /// `CODELENS` — waiting for the length/literal and distance code lengths.
    Codelens,
    /// `LEN_` — first entry into [`InflateMode::Len`].
    LenBegin,
    /// `LEN` — waiting for a length/literal/end-of-block code.
    Len,
    /// `LENEXT` — waiting for the length extra bits.
    Lenext,
    /// `DIST` — waiting for a distance code.
    Dist,
    /// `DISTEXT` — waiting for the distance extra bits.
    Distext,
    /// `MATCH` — waiting for output space to copy a matched string.
    Match,
    /// `LIT` — waiting for output space to write a literal.
    Lit,
    /// `CHECK` — waiting for the 32-bit check value.
    Check,
    /// `LENGTH` — waiting for the 32-bit gzip uncompressed length.
    Length,
    /// `DONE` — finished; remains here until reset.
    Done,
    /// `BAD` — a data error occurred; remains here until reset.
    Bad,
    /// `MEM` — an allocation error occurred; remains here until reset.
    Mem,
    /// `SYNC` — scanning for synchronization bytes to restart inflate.
    Sync,
}

/// Owned decompressor state — the safe-Rust analogue of the C `inflate_state`
/// struct (`inflate.h`).
///
/// Buffers that the C code allocates via `ZALLOC` (the sliding `window`) are
/// owned [`Vec`]s, released automatically by `Drop`. The C `code *` cursors
/// into the `codes[]` table (`lencode`, `distcode`, `next`) are represented as
/// `usize` offsets into [`codes`](InflateState::codes), avoiding raw pointers
/// while preserving the exact addressing semantics.
pub struct InflateState {
    /// Current decode mode (the C `state->mode`).
    pub mode: InflateMode,
    /// `true` while processing the last block (C `last`).
    pub last: bool,
    /// Wrapper selector: bit 0 = zlib, bit 1 = gzip, bit 2 = validate check
    /// value (C `wrap`).
    pub wrap: i32,
    /// `true` once a preset dictionary has been provided (C `havedict`).
    pub havedict: bool,
    /// gzip header method/flags, `0` for zlib, or `-1` for raw / no header yet
    /// (C `flags`).
    pub flags: i32,
    /// zlib-header maximum back-reference distance (C `dmax`, INFLATE_STRICT).
    pub dmax: u32,
    /// Protected copy of the running check value (C `check`).
    pub check: u32,
    /// Protected copy of the total output byte count (C `total`).
    pub total: u64,
    /// Caller-provided destination for parsed gzip header fields
    /// (C `gz_headerp head`); [`None`] when no header capture was requested.
    pub head: Option<GzHeader>,

    // --- sliding window ---
    /// Base-2 log of the requested window size (C `wbits`).
    pub wbits: u32,
    /// Window size, or `0` if the window is not yet in use (C `wsize`).
    pub wsize: u32,
    /// Number of valid bytes currently in the window (C `whave`).
    pub whave: u32,
    /// Window write cursor (C `wnext`).
    pub wnext: u32,
    /// The allocated sliding window (C `window`); empty until first needed.
    pub window: Vec<u8>,

    // --- bit accumulator ---
    /// Input bit accumulator (C `hold`); a `u64` holds the widest fill the
    /// decoder ever needs.
    pub hold: u64,
    /// Number of valid bits currently in [`hold`](InflateState::hold)
    /// (C `bits`).
    pub bits: u32,

    // --- string / stored-block copying ---
    /// Literal byte, or length of data to copy (C `length`).
    pub length: u32,
    /// Back-reference distance to copy a string from (C `offset`).
    pub offset: u32,

    // --- table / code decoding ---
    /// Number of extra bits currently needed (C `extra`).
    pub extra: u32,

    // --- fixed and dynamic code tables ---
    /// Offset into [`codes`](InflateState::codes) of the length/literal decode
    /// table (the C `code *lencode` cursor).
    pub lencode: usize,
    /// Offset into [`codes`](InflateState::codes) of the distance decode table
    /// (the C `code *distcode` cursor).
    pub distcode: usize,
    /// Index bits for the length/literal table (C `lenbits`).
    pub lenbits: u32,
    /// Index bits for the distance table (C `distbits`).
    pub distbits: u32,

    // --- dynamic table building ---
    /// Number of code-length code lengths (C `ncode`).
    pub ncode: u32,
    /// Number of length-code lengths (C `nlen`).
    pub nlen: u32,
    /// Number of distance-code lengths (C `ndist`).
    pub ndist: u32,
    /// Number of code lengths gathered so far in [`lens`](InflateState::lens)
    /// (C `have`).
    pub have: u32,
    /// Offset of the next free slot in [`codes`](InflateState::codes)
    /// (the C `code *next` cursor).
    pub next: usize,
    /// Temporary storage for code lengths (C `lens[320]`).
    pub lens: [u16; 320],
    /// Work area for building the code tables (C `work[288]`).
    pub work: [u16; 288],
    /// Space for the constructed decode tables (C `codes[ENOUGH]`).
    pub codes: [Code; ENOUGH],

    /// If `false`, allow an out-of-range back-reference distance
    /// (C `sane`, INFLATE_ALLOW_INVALID_DISTANCE_TOOFAR).
    pub sane: bool,
    /// Bits back of the last unprocessed length/literal code (C `back`).
    pub back: i32,
    /// Initial length of the current match (C `was`).
    pub was: u32,
}

impl InflateState {
    /// Create a fresh decompressor state for the given `window_bits`.
    ///
    /// All scalars are zeroed, the mode machine starts at
    /// [`InflateMode::Head`], `sane` is enabled, and the sliding window is left
    /// unallocated (an empty [`Vec`]) until the decoder first needs it —
    /// mirroring the C `inflateReset`/`inflateReset2` initial state. No decode
    /// work is performed here.
    #[must_use]
    pub fn new(window_bits: u32) -> Self {
        Self {
            mode: InflateMode::Head,
            last: false,
            wrap: 0,
            havedict: false,
            flags: 0,
            dmax: 0,
            check: 0,
            total: 0,
            head: None,

            wbits: window_bits,
            wsize: 0,
            whave: 0,
            wnext: 0,
            window: Vec::new(),

            hold: 0,
            bits: 0,

            length: 0,
            offset: 0,

            extra: 0,

            lencode: 0,
            distcode: 0,
            lenbits: 0,
            distbits: 0,

            ncode: 0,
            nlen: 0,
            ndist: 0,
            have: 0,
            next: 0,
            lens: [0; 320],
            work: [0; 288],
            codes: core::array::from_fn(|_| Code::default()),

            sane: true,
            back: 0,
            was: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn head_mode_uses_the_c_sentinel_discriminant() {
        // The base sentinel must match C `HEAD = 16180`; the following variants
        // increment from there, exactly as the C enum does.
        assert_eq!(InflateMode::Head as i32, 16180);
        assert_eq!(InflateMode::Flags as i32, 16181);
        assert_eq!(InflateMode::Sync as i32, 16180 + 31);
    }

    #[test]
    fn new_starts_at_head_with_an_unallocated_window() {
        let state = InflateState::new(15);
        assert_eq!(state.mode, InflateMode::Head);
        assert_eq!(state.wbits, 15);
        assert_eq!(state.wsize, 0);
        assert!(state.window.is_empty());
        assert!(state.sane);
        // The code-table scratch space is sized to the inflate `ENOUGH` budget.
        assert_eq!(state.codes.len(), ENOUGH);
        assert_eq!(state.lens.len(), 320);
        assert_eq!(state.work.len(), 288);
    }
}
