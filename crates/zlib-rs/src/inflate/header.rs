//! Container header parsing: the safe port of the eleven header states of
//! `inflate()` (`inflate.c` L506-L710) and of `inflateGetHeader`
//! (`inflate.c` L1219-L1231).
//!
//! This module is the first thing a stream's bytes touch, and every one of those
//! bytes is attacker-controlled. It decides three questions before any DEFLATE
//! data is decoded: whether the stream carries a zlib wrapper (RFC 1950), a gzip
//! wrapper (RFC 1952) or no wrapper at all; whether the wrapper is well formed;
//! and, for gzip, what to copy into the caller's `gz_header`.
//!
//! # The states, in the order the reference declares them
//!
//! Each handler below ports exactly one arm of `inflate()`'s `switch`, keeps the
//! arm's name, and cites the lines it came from. The `Advance` outcome is C's
//! `break` *and* C's `/* fallthrough */`: both simply re-dispatch on
//! [`InflateState::mode`], which the handler has already advanced.
//!
//! | State | Handler | `inflate.c` | Consumes |
//! |---|---|---|---|
//! | `HEAD` | [`head`] | L506-L553 | two bytes: gzip magic, or zlib `CMF`/`FLG` |
//! | `FLAGS` | [`flags`] | L555-L573 | gzip `CM` and `FLG` |
//! | `TIME` | [`time`] | L575-L581 | gzip `MTIME`, four bytes |
//! | `OS` | [`os`] | L584-L592 | gzip `XFL` and `OS` |
//! | `EXLEN` | [`ex_len`] | L595-L607 | gzip `XLEN`, two bytes, only if `FEXTRA` |
//! | `EXTRA` | [`extra`] | L609-L629 | `XLEN` bytes of extra field, resumable |
//! | `NAME` | [`name`] | L633-L651 | zero-terminated file name, resumable |
//! | `COMMENT` | [`comment`] | L655-L673 | zero-terminated comment, resumable |
//! | `HCRC` | [`hcrc`] | L676-L692 | gzip header CRC-16, only if `FHCRC` |
//! | `DICTID` | [`dict_id`] | L694-L698 | zlib dictionary id, four bytes |
//! | `DICT` | [`dict`] | L700-L706 | nothing; waits on the caller |
//!
//! The transition diagram at `inflate.h` L55-L78 is the authority on how they
//! chain: `HEAD` splits three ways into gzip, zlib and raw; the gzip chain runs
//! `FLAGS -> TIME -> OS -> EXLEN -> EXTRA -> NAME -> COMMENT -> HCRC -> TYPE`; the
//! zlib chain is `DICTID -> DICT -> TYPE` or straight to `TYPE`; and raw goes
//! directly to `TYPEDO`.
//!
//! # Resumability is the contract, not an optimisation
//!
//! `inflate()` promises that a caller may supply input one byte at a time, so
//! every state here has to be re-enterable at the exact point it stopped. The C
//! implementation achieves that with `NEEDBITS(n)`, whose `PULLBYTE` does
//! `goto inf_leave` when the input runs dry (`inflate.c` L356-L371); the bits
//! already pulled stay in `hold`, and the next call resumes in the same state.
//!
//! Three pieces of state carry the resume point, and all three live in
//! `crates/zlib-rs/src/inflate/state.rs`:
//!
//! * [`InflateState::mode`] -- which state to re-enter,
//! * `hold` and `bits` -- the partially filled accumulator, preserved by
//!   [`InflateState::need_bits`] whether it succeeds or not,
//! * `length` -- how much of the extra field is still outstanding, and the write
//!   cursor into the caller's name and comment buffers.
//!
//! Nothing else is saved, and nothing is consumed that cannot be used: a handler
//! that reports [`HeaderAction::NeedInput`] has advanced `next_in` only over
//! bytes whose bits are now in the accumulator.
//!
//! One consequence is easy to overlook. `NEEDBITS(32)` pulls bytes while
//! `bits < 32`, so a caller who primed the accumulator with
//! `inflatePrime` can leave `bits` *above* 32 -- up to 39. That is why the
//! accumulator is a `u64` in this port (see `state.rs`) and why every expression
//! below reads the accumulator at its full width instead of masking it to sixteen
//! or thirty-two bits: the reference reads `unsigned long hold` the same way, and
//! masking would change which streams are accepted.
//!
//! # How a handler talks to the driver
//!
//! C reaches its caller through one `z_streamp`. This port has no `z_stream`:
//! raw pointers stop at the facade (AAP §0.6.1), so the input arrives as a slice
//! plus a cursor and everything the reference writes to the stream comes back in
//! a [`HeaderExit`]:
//!
//! * `action` -- what the driver does next, one of the four things a C arm can do:
//!   fall through or `break` ([`HeaderAction::Advance`]), `goto inf_leave`
//!   ([`HeaderAction::NeedInput`]), enter `BAD` ([`HeaderAction::Bad`]), or
//!   `RESTORE(); return Z_NEED_DICT;` ([`HeaderAction::NeedDict`]).
//! * `adler` -- the value C assigns with `strm->adler = state->check = ...`, and
//!   *only* at the four sites that do so (L550, L690, L696 and L705). The header
//!   CRC accumulates into `state.check` at several other points without ever
//!   being published, so the driver must not mirror `check` unconditionally; that
//!   would let a partial header CRC show up in a caller's `adler` field.
//! * `msg` -- the text C stores in `strm->msg`, and nothing else. C sets no
//!   message on a successful state, and neither does this module.
//!
//! # The `done` tri-state
//!
//! `gz_header.done` (`zlib.h` L131-L132) is the caller's only way to tell three
//! outcomes apart, and all three are produced here:
//!
//! | Value | Meaning | Written at |
//! |---|---|---|
//! | `0` | no header read yet | [`inflate_get_header`], `inflate.c` L1229 |
//! | `-1` | this stream carries no gzip header | [`head`], `inflate.c` L522-L523 |
//! | `1` | the gzip header has been read in full | [`hcrc`], `inflate.c` L686-L689 |
//!
//! # ★ The bounded writes are a fixed vulnerability, not a nicety
//!
//! The three variable-length gzip fields are the historically most-exploited
//! surface in this library, and the shape of the guard at `inflate.c` L613-L621 is
//! the *fixed* form of a real overflow. It has three parts, all of which are
//! reproduced in [`extra`]: a header must be installed, its `extra` pointer must
//! be non-null, and the offset already reached must still be below the capacity
//! the caller advertised in `extra_max`. Only then is anything copied, and even
//! then the count is clamped to the space left. `name` and `comment` are guarded
//! the same way byte by byte (L639-L642, L661-L664).
//!
//! Two details of that clamping are load-bearing and are called out again at the
//! call sites, because getting either one wrong is silent:
//!
//! * the CRC is computed over **every byte consumed from the input**, never over
//!   the possibly shorter run that was actually stored, and
//! * `extra_len` reports the length the *stream* advertised, not the number of
//!   bytes stored, which is exactly how a caller detects that its buffer was too
//!   small.
//!
//! In this port the capacities cannot be circumvented even by a mistake here: the
//! caller's three buffers arrive as slices whose lengths *are* `extra_max`,
//! `name_max` and `comm_max`, and every write goes through the checked writers on
//! [`GzHeaderSink`]. There is no pointer to advance and no length to get wrong.
//!
//! # `wrap` and `flags` both mean more than they look like
//!
//! `wrap` is a bit set, not an enum (`inflate.h` L86-L87): bit 0 permits a zlib
//! header, bit 1 permits a gzip header, bit 2 requests check-value validation.
//! `inflateReset2` computes it as `(windowBits >> 4) + 5` (`inflate.c` L152),
//! which is why every wrapped stream starts out validating, and `inflateValidate`
//! can clear bit 2 afterwards (L1384-L1394). Every CRC step and the header CRC
//! comparison in this module are gated on bit 2 -- with one deliberate exception,
//! noted in [`head`], where the reference folds the gzip magic into the CRC
//! unconditionally.
//!
//! `flags` is a tri-state plus a payload (`inflate.h` L89-L90): `-1` means "raw,
//! or no header seen yet", `0` means "a zlib header was read", and any other value
//! is the gzip `CM`/`FLG` word itself. The distinction matters outside this
//! module: `inflate()` picks CRC-32 over Adler-32 when `flags` is non-zero
//! (L302-L303), and `inflateSync` treats a stream as raw when `flags` is still
//! `-1` (L1299). So `flags = 0` in [`head`] and `flags = <FLG word>` in [`flags`]
//! are two different, meaningful assignments and must not be collapsed.
//!
//! # Not gated behind a feature
//!
//! gzip decoding is unconditional here. `inflate.h` L11-L17 defines `GUNZIP`
//! unless `NO_GZIP` is set and states plainly that "for shared libraries, gzip
//! decoding should be left enabled"; the shipped build leaves it enabled, so
//! there is no `#[cfg]` on any state below and `crates/zlib-rs/Cargo.toml`
//! declares no feature that could remove one.
//!
//! # Messages are `&'static str`, and NUL termination is the facade's business
//!
//! The five texts below are what a caller reads out of `z_stream.msg`, and
//! `test/infcover.c` prints them, so they are reproduced character for character
//! and at the same decision points. They are `&'static str` rather than
//! NUL-terminated byte literals for one decisive reason: that is the type of the
//! message channel this subsystem already uses -- `StreamReset::msg` and
//! `inffast::FastExit::msg` are both `Option<&'static str>` -- so a
//! NUL-terminated form could not travel through it without a second
//! representation to keep in step. `crates/zlib-rs/src/error.rs` states the same
//! division of labour for the `z_errmsg` table: turning a message into a
//! `char *` belongs to `crates/libz-rs-sys`, which needs no allocation to do it
//! because these are compile-time constants with `'static` lifetime and a
//! one-to-one static counterpart on its side.
//!
//! # Safety and failure posture
//!
//! No `unsafe`, no raw pointer, no `std`, no allocation, and nothing that can
//! panic on any input. Concretely: no slice is indexed, every narrowing goes
//! through a total helper, and every arithmetic step that could leave its domain
//! is checked, saturating or deliberately wrapping to match C's unsigned
//! arithmetic. A malformed header always ends in [`Mode::Bad`], which the driver
//! turns into `Z_DATA_ERROR`, or in a clean request for more input; it never ends
//! in an abort. `cargo fuzz run fuzz_inflate` is the gate on that claim, and the
//! deterministic smoke test at the bottom of this file is its cheap standing
//! version.
//!
//! # What lives here, and what deliberately does not
//!
//! `DICTID` and `DICT` are header states in the reference's own layout, so they
//! are ported here rather than in the driver, and [`inflate_get_header`] is
//! likewise here because it exists only to install the sink these states fill.
//! Neither is duplicated in `crates/zlib-rs/src/inflate/mod.rs`. What is *not*
//! here: the `z_stream`-shaped mirror of `gz_header`, which is ABI-visible and
//! therefore belongs to `crates/libz-rs-sys/src/types.rs`; the dictionary
//! *content* check, which `inflateSetDictionary` performs (`inflate.c`
//! L1200-L1205); and the trailer states `CHECK` and `LENGTH`, which validate the
//! check value this module only ever initialises.

// `HeaderExit`, `HeaderAction` and the message constants all begin with the
// module's own name, which is the clearest way to name them at their use sites in
// the driver. `state.rs` and `config.rs` relax the same lint for the same reason.
#![allow(clippy::module_name_repetitions)]

// `Allocator` is not a dependency of this module's logic -- nothing here
// allocates -- but it is part of the public signature of `InflateState`, whose
// allocator parameter stands in for C's `z_streamp` back-pointer (`inflate.h`
// L83). Naming the state therefore means naming the bound, exactly as
// `inflate/inffast.rs` L288 and `inflate/window.rs` L165 already do.
use crate::adler32::ADLER32_INITIAL_VALUE;
use crate::allocate::Allocator;
use crate::crc32::crc32;
use crate::error::ReturnCode;
use crate::inflate::mode::Mode;
use crate::inflate::state::{GzHeaderSink, InflateState, DMAX_DEFAULT, GZ_HEADER_PENDING};

// -----------------------------------------------------------------------------
//  Error messages
// -----------------------------------------------------------------------------
//
// Five texts across six decision points: the zlib and gzip paths reject an
// unknown compression method with the *same* text at *different* sites, and both
// sites are reproduced.

/// `strm->msg` when the two zlib header bytes are not a multiple of 31, or when a
/// zlib header is not permitted at all (`inflate.c` L529).
///
/// RFC 1950 §2.2 requires that `CMF * 256 + FLG` be a multiple of 31; the check
/// bits that make it so are `FCHECK`, the low five bits of `FLG`.
pub(crate) const MSG_INCORRECT_HEADER_CHECK: &str = "incorrect header check";

/// `strm->msg` when the compression method is not `Z_DEFLATED`
/// (`inflate.c` L534 on the zlib path, L559 on the gzip path).
///
/// One text, two sites. `Z_DEFLATED` is the only method either container format
/// defines, so both paths reject everything else, and both must report it
/// identically.
pub(crate) const MSG_UNKNOWN_COMPRESSION_METHOD: &str = "unknown compression method";

/// `strm->msg` when a zlib header's window is larger than 32 KiB or larger than
/// the caller asked for (`inflate.c` L543).
pub(crate) const MSG_INVALID_WINDOW_SIZE: &str = "invalid window size";

/// `strm->msg` when a gzip header sets one of the three reserved `FLG` bits
/// (`inflate.c` L564).
///
/// RFC 1952 §2.3.1 reserves bits 5, 6 and 7 of `FLG` and requires a decompressor
/// not to attempt to decompress a member with any of them set.
pub(crate) const MSG_UNKNOWN_HEADER_FLAGS_SET: &str = "unknown header flags set";

/// `strm->msg` when the gzip header's own CRC-16 does not match
/// (`inflate.c` L680).
pub(crate) const MSG_HEADER_CRC_MISMATCH: &str = "header crc mismatch";

// -----------------------------------------------------------------------------
//  Format constants
// -----------------------------------------------------------------------------

/// The gzip magic `1f 8b` as the accumulator holds it (`inflate.c` L513).
///
/// `ID1` is `0x1f` and `ID2` is `0x8b` (RFC 1952 §2.3.1), read in that order into
/// an accumulator that fills from the low end, so the two bytes together read
/// `0x8b1f`. The constant is deliberately written the way the reference writes
/// it, byte-swapped-looking and all, so the two can be compared by eye.
const GZIP_MAGIC: u64 = 0x8b1f;

/// `Z_DEFLATED` (`zlib.h` L213): the only compression method either container
/// may name.
const Z_DEFLATED: i32 = 8;

/// `MAX_WBITS` (`zconf.h` L287): the largest window exponent, 15, meaning 32 KiB.
///
/// Used twice, for two different purposes: as the exponent a gzip stream is
/// assumed to need when the caller did not name one (`inflate.c` L514-L515), and
/// as the upper bound a zlib header's `CINFO` field may not exceed
/// (`inflate.c` L542).
const MAX_WBITS: u32 = 15;

/// `FDICT` as it appears *after* `DROPBITS(4)` has shifted the zlib header down
/// (`inflate.c` L551).
///
/// ★ This is why the reference tests `0x200` rather than `0x2000`. `FDICT` is bit
/// 5 of `FLG` (RFC 1950 §2.2), and `FLG` is the high byte of the sixteen-bit
/// value in the accumulator, so the bit starts out at position 13. By the time
/// the mode is chosen, the four `CM` bits have been dropped and it has moved down
/// to position 9.
const FDICT_SHIFTED: u64 = 0x200;

/// `FHCRC`, bit 1 of the gzip `FLG` byte, in the `CM`/`FLG` word `flags` holds
/// (`inflate.c` L570).
///
/// Set means a CRC-16 of the header precedes the compressed data
/// (RFC 1952 §2.3.1).
const FHCRC: i32 = 0x0200;

/// `FEXTRA`, bit 2 of `FLG`: an extra field is present (`inflate.c` L596).
const FEXTRA: i32 = 0x0400;

/// `FNAME`, bit 3 of `FLG`: a zero-terminated file name is present
/// (`inflate.c` L634).
const FNAME: i32 = 0x0800;

/// `FCOMMENT`, bit 4 of `FLG`: a zero-terminated comment is present
/// (`inflate.c` L656).
const FCOMMENT: i32 = 0x1000;

/// The three reserved `FLG` bits, 5 to 7, which RFC 1952 §2.3.1 requires to be
/// zero (`inflate.c` L563).
const FRESERVED: i32 = 0xe000;

/// The number of bits a two-byte header field needs: `NEEDBITS(16)`.
const FIELD_BITS_16: u32 = 16;

/// The number of bits a four-byte header field needs: `NEEDBITS(32)`.
const FIELD_BITS_32: u32 = 32;

/// The width of the `CM` nibble and of `CINFO`, and hence the `DROPBITS(4)` count
/// (`inflate.c` L533-L539).
const NIBBLE_BITS: u32 = 4;

/// The mask of one nibble, for `BITS(4)`.
const NIBBLE_MASK: u64 = 0xf;

/// The mask of one byte, for `BITS(8)` and for the `XFL` field.
const BYTE_MASK: u64 = 0xff;

/// The smallest window exponent a zlib header can encode: `CINFO` of zero means
/// 256 bytes, so `len = BITS(4) + 8` (`inflate.c` L539).
const WBITS_BIAS: u32 = 8;

/// The mask of the sixteen bits a gzip header CRC-16 is compared over
/// (`inflate.c` L679).
const CRC16_MASK: u32 = 0xffff;

// -----------------------------------------------------------------------------
//  Total narrowing helpers
// -----------------------------------------------------------------------------

/// The low 32 bits of an accumulator value.
///
/// Every gzip and zlib header field is at most four bytes wide, so this loses
/// nothing that the reference keeps: C reads the same fields out of an
/// `unsigned long` through casts to `unsigned` and `int`. The point of routing
/// them through one helper is that no narrowing cast, and nothing that can
/// panic, appears anywhere else in this module.
///
/// The `unwrap_or` arm is unreachable -- the mask guarantees the value fits --
/// and exists so that the absence of a panicking path is structural rather than
/// merely argued. `state.rs` narrows the same way in `hold_low32`.
#[must_use]
fn low_u32(value: u64) -> u32 {
    u32::try_from(value & u64::from(u32::MAX)).unwrap_or(0)
}

/// Reinterprets a 32-bit field as C's `int`, bit for bit.
///
/// This is the `(int)` cast at `inflate.c` L557, L586, L587 and L599. Values that
/// large cannot occur in a well-formed header, but `inflatePrime` lets a caller
/// leave arbitrary high bits in the accumulator, and a bit-preserving conversion
/// is what a C compiler performs there. Going through `to_ne_bytes` rather than
/// `as` keeps the conversion explicit and total.
#[must_use]
fn as_i32(value: u32) -> i32 {
    i32::from_ne_bytes(value.to_ne_bytes())
}

// -----------------------------------------------------------------------------
//  Header CRC helpers -- `CRC2` and `CRC4` (`inflate.c` L308-L325)
// -----------------------------------------------------------------------------

/// Folds the low two bytes of `word`, least significant first, into `check`.
///
/// The port of the `CRC2` macro (`inflate.c` L310-L315):
///
/// ```c
/// #define CRC2(check, word) \
///     do { \
///         hbuf[0] = (unsigned char)(word); \
///         hbuf[1] = (unsigned char)((word) >> 8); \
///         check = crc32(check, hbuf, 2); \
///     } while (0)
/// ```
///
/// The byte order is the wire order: a gzip header CRC-16 covers the header bytes
/// as they appear in the stream, and the accumulator filled from the low end, so
/// spilling it little-endian reproduces those bytes exactly.
#[must_use]
pub(crate) fn crc2(check: u32, word: u64) -> u32 {
    let [low, high, _, _] = low_u32(word).to_le_bytes();
    crc32(check, &[low, high])
}

/// Folds all four low bytes of `word`, least significant first, into `check`.
///
/// The port of the `CRC4` macro (`inflate.c` L317-L324), used for the four-byte
/// `MTIME` field and identical to [`crc2`] but for the width.
#[must_use]
pub(crate) fn crc4(check: u32, word: u64) -> u32 {
    crc32(check, &low_u32(word).to_le_bytes())
}

/// Whether header bytes should be folded into the header CRC.
///
/// The gate `(state->flags & 0x0200) && (state->wrap & 4)`, which the reference
/// repeats verbatim at L570, L579, L590, L601, L622, L644 and L666: the stream
/// must have announced a header CRC, *and* the caller must not have switched
/// check-value validation off with `inflateValidate`.
#[must_use]
fn header_crc_enabled<'a, A: Allocator<'a>>(state: &InflateState<'a, A>) -> bool {
    (state.flags & FHCRC) != 0 && state.wrap.verifies_check_value()
}

// -----------------------------------------------------------------------------
//  The driver protocol
// -----------------------------------------------------------------------------

/// What the driver must do when a header state returns.
///
/// The four possibilities are exactly the four ways an arm of `inflate()`'s
/// `switch` can end, so a driver that handles all four handles everything the
/// reference does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HeaderAction {
    /// C's `break` out of the `switch`, and equally C's `/* fallthrough */`.
    ///
    /// The handler has already advanced [`InflateState::mode`], so the driver
    /// simply dispatches again. The two C spellings differ only in whether the
    /// next arm is reached without re-testing the loop, which is unobservable.
    Advance,

    /// C's `goto inf_leave`: the field is incomplete and needs more input.
    ///
    /// [`InflateState::mode`] is unchanged, the accumulator holds whatever bits
    /// were available, and `next_in` has advanced only over bytes that are now in
    /// the accumulator or already copied. The driver runs its epilogue and
    /// returns to the caller; the next call resumes in the same state.
    NeedInput,

    /// C's `state->mode = BAD`: the header is malformed.
    ///
    /// The handler has already set [`Mode::Bad`], and [`HeaderExit::msg`] carries
    /// the text for `strm->msg`. The driver's `BAD` arm turns this into
    /// [`ReturnCode::DATA_ERROR`] (`inflate.c` L1114-L1116).
    Bad,

    /// C's `RESTORE(); return Z_NEED_DICT;` (`inflate.c` L701-L704).
    ///
    /// The stream needs a preset dictionary. [`InflateState::mode`] stays
    /// [`Mode::Dict`], no further input is consumed, and the driver returns
    /// [`ReturnCode::NEED_DICT`] *without* running the ordinary epilogue -- which
    /// is what keeps the dictionary id this state published in `strm->adler`
    /// visible to the caller, and what `inflateSetDictionary` then checks
    /// against (`inflate.c` L1200-L1205).
    NeedDict,
}

/// Everything a header state writes back that is not already in the state.
///
/// The counterpart of `inflate/inffast.rs`'s `FastExit`, and for the same reason:
/// C writes `strm->msg` and `strm->adler` through a `z_streamp` this crate does
/// not have, so those two values travel back to the driver, which owns the
/// stream-facing side.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HeaderExit {
    /// What the driver does next.
    pub(crate) action: HeaderAction,

    /// The value to publish in `strm->adler`, or [`None`] to leave it alone.
    ///
    /// [`Some`] at exactly the four sites where C writes
    /// `strm->adler = state->check = ...`: the end of a valid zlib header
    /// (L550), the end of a gzip header (L690), the dictionary id (L696) and the
    /// dictionary hand-off (L705). Every other change to `state.check` in this
    /// module accumulates the *header* CRC, which the reference never publishes.
    pub(crate) adler: Option<u32>,

    /// The text for `strm->msg`, [`Some`] exactly when `action` is
    /// [`HeaderAction::Bad`].
    ///
    /// Always one of the five `MSG_*` constants in this module. C leaves `msg`
    /// untouched on a successful state, so [`None`] here means "do not write",
    /// not "write an empty string".
    pub(crate) msg: Option<&'static str>,
}

impl HeaderExit {
    /// C's `break`: keep dispatching, publish nothing.
    #[must_use]
    const fn advance() -> Self {
        Self {
            action: HeaderAction::Advance,
            adler: None,
            msg: None,
        }
    }

    /// C's `break` preceded by `strm->adler = state->check = adler`.
    #[must_use]
    const fn advance_with_adler(adler: u32) -> Self {
        Self {
            action: HeaderAction::Advance,
            adler: Some(adler),
            msg: None,
        }
    }

    /// C's `goto inf_leave` from inside `PULLBYTE`.
    #[must_use]
    const fn need_input() -> Self {
        Self {
            action: HeaderAction::NeedInput,
            adler: None,
            msg: None,
        }
    }

    /// C's `RESTORE(); return Z_NEED_DICT;`.
    #[must_use]
    const fn need_dict() -> Self {
        Self {
            action: HeaderAction::NeedDict,
            adler: None,
            msg: None,
        }
    }
}

/// Enters [`Mode::Bad`] with `msg`, the way every rejection in `inflate()` does.
///
/// The reference spells this out at each site as `strm->msg = "..."` followed by
/// `state->mode = BAD; break;`. Routing all six rejections through one helper is
/// what guarantees the two halves can never come apart -- a `Bad` outcome whose
/// mode was left behind would leave the stream decoding a header it had already
/// rejected.
fn reject<'a, A: Allocator<'a>>(state: &mut InflateState<'a, A>, msg: &'static str) -> HeaderExit {
    state.mode = Mode::Bad;
    HeaderExit {
        action: HeaderAction::Bad,
        adler: None,
        msg: Some(msg),
    }
}

// -----------------------------------------------------------------------------
//  `HEAD` -- `inflate.c` L506-L553
// -----------------------------------------------------------------------------

/// Reads the first two bytes and decides what kind of stream this is.
///
/// The port of the `HEAD` arm (`inflate.c` L506-L553), which is the only state
/// that can reach three different successors:
///
/// * `wrap == 0` -- raw DEFLATE, so there is no header to read at all and the
///   stream starts at [`Mode::TypeDo`] (L507-L510).
/// * the two bytes are the gzip magic and bit 1 of `wrap` permits gzip -- start
///   the gzip chain at [`Mode::Flags`] (L513-L521).
/// * otherwise the bytes must be a valid zlib header, and the stream continues at
///   [`Mode::DictId`] or [`Mode::Type`] depending on `FDICT` (L524-L553).
///
/// # Order of operations that matters
///
/// * The gzip magic is folded into `state.check` **unconditionally** (L516-L517):
///   neither the `FHCRC` bit nor `wrap & 4` is consulted, because the flags have
///   not been read yet. Every *later* fold is gated by [`header_crc_enabled`]. If
///   the stream turns out to have no header CRC the accumulated value is simply
///   never compared, and `HCRC` overwrites it (L690).
/// * `head->done = -1` is written **before** the zlib validation (L522-L523), so a
///   caller that installed a header learns "not gzip" even when the stream then
///   turns out not to be valid zlib either.
/// * `wbits` is taken from the header **before** the window-size check
///   (L540-L546), so a rejected stream can leave a `wbits` above 15 behind. That
///   is harmless -- the stream is in [`Mode::Bad`] and needs a reset before it can
///   be used again -- and reproducing the order keeps the two implementations
///   diffable.
pub(crate) fn head<'a, A: Allocator<'a>>(
    state: &mut InflateState<'a, A>,
    input: &[u8],
    next_in: &mut usize,
) -> HeaderExit {
    // L507-L510: `if (state->wrap == 0) { state->mode = TYPEDO; break; }`.
    if state.wrap.is_raw() {
        state.mode = Mode::TypeDo;
        return HeaderExit::advance();
    }

    // L511: `NEEDBITS(16)`.
    if !state.need_bits(FIELD_BITS_16, input, next_in) {
        return HeaderExit::need_input();
    }

    // L513-L521: `if ((state->wrap & 2) && hold == 0x8b1f)`. The comparison is
    // against the whole accumulator, as in C, so a primed accumulator with
    // leftover high bits is not mistaken for a gzip stream.
    if state.wrap.allows_gzip_header() && state.hold == GZIP_MAGIC {
        // L514-L515: a gzip member has no window field, so the maximum is assumed
        // when the caller did not name one.
        if state.wbits == 0 {
            state.wbits = MAX_WBITS;
        }
        // L516: `state->check = crc32(0L, Z_NULL, 0)`, which is 0. Passing an
        // empty slice yields the same 0 here, so the call is exact -- unlike the
        // Adler-32 initial value below, where a null pointer and an empty slice
        // differ.
        state.check = crc32(0, &[]);
        // L517: `CRC2(state->check, hold)` -- the magic is part of the header CRC.
        state.check = crc2(state.check, state.hold);
        state.init_bits();
        state.mode = Mode::Flags;
        return HeaderExit::advance();
    }

    // L522-L523: `if (state->head != Z_NULL) state->head->done = -1;`.
    if let Some(sink) = state.head.as_mut() {
        sink.mark_absent();
    }

    // L524-L532: `if (!(state->wrap & 1) || ((BITS(8) << 8) + (hold >> 8)) % 31)`.
    //
    // ★ Note the operand order. `hold` holds `CMF` in its low byte and `FLG` in
    // its high byte, because the accumulator fills from the low end, whereas
    // RFC 1950 §2.2 defines the check over `CMF * 256 + FLG`. Moving the low byte
    // up and the high byte down is what turns one into the other. The whole
    // expression is evaluated at accumulator width, exactly as C evaluates it in
    // `unsigned long`, and cannot overflow: the left term is at most 0xff00 and
    // the right at most 2**31.
    let cmf = state.hold & BYTE_MASK;
    let header_check = (cmf << 8) + (state.hold >> 8);
    if !state.wrap.allows_zlib_header() || header_check % 31 != 0 {
        return reject(state, MSG_INCORRECT_HEADER_CHECK);
    }

    // L533-L537: `if (BITS(4) != Z_DEFLATED)`.
    if as_i32(low_u32(state.hold & NIBBLE_MASK)) != Z_DEFLATED {
        return reject(state, MSG_UNKNOWN_COMPRESSION_METHOD);
    }

    // L538: `DROPBITS(4)`. Sixteen bits are live, so this always succeeds; the
    // guard is here so that a corrupted accumulator cannot make the `CINFO` read
    // below use unshifted bits.
    if !state.drop_bits(NIBBLE_BITS) {
        return HeaderExit::need_input();
    }

    // L539-L541: `len = BITS(4) + 8; if (state->wbits == 0) state->wbits = len;`.
    // `CINFO` is a nibble, so `len` is 8..=23 and the addition cannot overflow.
    let len = low_u32(state.hold & NIBBLE_MASK) + WBITS_BIAS;
    if state.wbits == 0 {
        state.wbits = len;
    }

    // L542-L546: `if (len > 15 || len > state->wbits)`.
    //
    // ★ Both halves matter. The first rejects a window larger than the format
    // allows; the second rejects a stream that needs a larger window than the
    // caller asked for, which is the case `test/infcover.c` L403 covers with
    // `inflateInit2(&strm, 8)` against a `78 9c` header.
    if len > MAX_WBITS || len > state.wbits {
        return reject(state, MSG_INVALID_WINDOW_SIZE);
    }

    // L547-L548. `len <= 15` here, so the shift is always in range; the fallback
    // is the same 32 KiB maximum the state starts out with.
    state.dmax = 1_u32.checked_shl(len).unwrap_or(DMAX_DEFAULT);
    state.flags = 0;

    // L550: `strm->adler = state->check = adler32(0L, Z_NULL, 0)`.
    //
    // ★ That call returns **1**, not 0: `adler32` answers a null buffer with the
    // required initial value (`adler32.c` L128-L130 forwarding to the
    // `buf == Z_NULL` arm of `adler32_z`). An empty slice is not `Z_NULL`, so
    // `adler32(0, &[])` would return 0 and silently break every zlib check value.
    // `ADLER32_INITIAL_VALUE` exists for exactly these internal call sites.
    state.check = ADLER32_INITIAL_VALUE;

    // L551-L552: `state->mode = hold & 0x200 ? DICTID : TYPE; INITBITS();`.
    state.mode = if (state.hold & FDICT_SHIFTED) != 0 {
        Mode::DictId
    } else {
        Mode::Type
    };
    state.init_bits();
    HeaderExit::advance_with_adler(state.check)
}

// -----------------------------------------------------------------------------
//  `FLAGS` -- `inflate.c` L555-L573
// -----------------------------------------------------------------------------

/// Reads the gzip `CM` and `FLG` bytes.
///
/// The port of the `FLAGS` arm (`inflate.c` L555-L573). The two bytes are stored
/// whole in `state.flags`, which is what makes the later states' `flags & 0x0400`
/// style tests work, and what tells `inflate()` to use CRC-32 rather than
/// Adler-32 for the body (L302-L303).
///
/// ★ The rejection at L559 uses the same text as the zlib path's L534 but is a
/// different decision point, over a different field: here the method is the low
/// byte of the `CM`/`FLG` word, there it was the low nibble of `CMF`. Both are
/// reproduced, because a test that distinguishes them -- `test/infcover.c` L399
/// against L401 -- exists.
pub(crate) fn flags<'a, A: Allocator<'a>>(
    state: &mut InflateState<'a, A>,
    input: &[u8],
    next_in: &mut usize,
) -> HeaderExit {
    // L556: `NEEDBITS(16)`.
    if !state.need_bits(FIELD_BITS_16, input, next_in) {
        return HeaderExit::need_input();
    }
    let word = state.hold;

    // L557: `state->flags = (int)(hold)`.
    state.flags = as_i32(low_u32(word));

    // L558-L562: `if ((state->flags & 0xff) != Z_DEFLATED)`.
    if (state.flags & 0xff) != Z_DEFLATED {
        return reject(state, MSG_UNKNOWN_COMPRESSION_METHOD);
    }

    // L563-L567: `if (state->flags & 0xe000)` -- the reserved bits must be zero.
    if (state.flags & FRESERVED) != 0 {
        return reject(state, MSG_UNKNOWN_HEADER_FLAGS_SET);
    }

    // L568-L569: `state->head->text = (int)((hold >> 8) & 1)`. `FTEXT` is bit 0 of
    // `FLG`, which is the high byte of this word.
    let text = ((word >> 8) & 1) != 0;
    if let Some(sink) = state.head.as_mut() {
        sink.text = text;
    }

    // L570-L571: the first *gated* fold. `state.flags` has just been assigned, so
    // this is the first point at which the gate can be evaluated at all.
    if header_crc_enabled(state) {
        state.check = crc2(state.check, word);
    }

    // L572-L574: `INITBITS(); state->mode = TIME;` then fall through.
    state.init_bits();
    state.mode = Mode::Time;
    HeaderExit::advance()
}

// -----------------------------------------------------------------------------
//  `TIME` -- `inflate.c` L575-L581
// -----------------------------------------------------------------------------

/// Reads the gzip `MTIME` field.
///
/// The port of the `TIME` arm (`inflate.c` L575-L581). `MTIME` is the modification
/// time as a four-byte little-endian Unix timestamp (RFC 1952 §2.3.1), and this is
/// one of the two `NEEDBITS(32)` sites in the header -- the other being
/// [`dict_id`]. Both are why the accumulator is 64 bits wide in this port.
pub(crate) fn time<'a, A: Allocator<'a>>(
    state: &mut InflateState<'a, A>,
    input: &[u8],
    next_in: &mut usize,
) -> HeaderExit {
    // L576: `NEEDBITS(32)`.
    if !state.need_bits(FIELD_BITS_32, input, next_in) {
        return HeaderExit::need_input();
    }
    let word = state.hold;

    // L577-L578: `state->head->time = hold`.
    let time = low_u32(word);
    if let Some(sink) = state.head.as_mut() {
        sink.time = time;
    }

    // L579-L580: `CRC4(state->check, hold)` -- four bytes this time.
    if header_crc_enabled(state) {
        state.check = crc4(state.check, word);
    }

    // L581-L583.
    state.init_bits();
    state.mode = Mode::Os;
    HeaderExit::advance()
}

// -----------------------------------------------------------------------------
//  `OS` -- `inflate.c` L584-L592
// -----------------------------------------------------------------------------

/// Reads the gzip `XFL` and `OS` bytes.
///
/// The port of the `OS` arm (`inflate.c` L584-L592). `XFL` records the compressor
/// setting and `OS` the filesystem the member was made on (RFC 1952 §2.3.1).
///
/// Note that `os` is assigned from `hold >> 8` without a mask, exactly as at
/// L587: with sixteen bits live that is the second byte, and with a primed
/// accumulator it can be larger -- which is what the reference does too.
pub(crate) fn os<'a, A: Allocator<'a>>(
    state: &mut InflateState<'a, A>,
    input: &[u8],
    next_in: &mut usize,
) -> HeaderExit {
    // L585: `NEEDBITS(16)`.
    if !state.need_bits(FIELD_BITS_16, input, next_in) {
        return HeaderExit::need_input();
    }
    let word = state.hold;

    // L586-L589.
    let xflags = as_i32(low_u32(word & BYTE_MASK));
    let operating_system = as_i32(low_u32(word >> 8));
    if let Some(sink) = state.head.as_mut() {
        sink.xflags = xflags;
        sink.os = operating_system;
    }

    // L590-L591.
    if header_crc_enabled(state) {
        state.check = crc2(state.check, word);
    }

    // L592-L594.
    state.init_bits();
    state.mode = Mode::ExLen;
    HeaderExit::advance()
}

// -----------------------------------------------------------------------------
//  `EXLEN` -- `inflate.c` L595-L607
// -----------------------------------------------------------------------------

/// Reads the gzip `XLEN` field, or records that there is no extra field.
///
/// The port of the `EXLEN` arm (`inflate.c` L595-L607). Two things happen here
/// that the next state depends on:
///
/// * `state.length` is seeded with the advertised field length and becomes
///   [`extra`]'s resume counter (L598),
/// * `extra_len` is reported to the caller *unclamped* (L599-L600), which is how a
///   caller detects that its `extra_max` was too small.
///
/// When `FEXTRA` is clear the reference does not merely skip the field: it clears
/// the caller's pointer with `state->head->extra = Z_NULL` (L605-L606). That is
/// observable, so [`GzHeaderSink::clear_extra`] reproduces it.
pub(crate) fn ex_len<'a, A: Allocator<'a>>(
    state: &mut InflateState<'a, A>,
    input: &[u8],
    next_in: &mut usize,
) -> HeaderExit {
    // L596: `if (state->flags & 0x0400)`.
    if (state.flags & FEXTRA) != 0 {
        // L597: `NEEDBITS(16)`.
        if !state.need_bits(FIELD_BITS_16, input, next_in) {
            return HeaderExit::need_input();
        }
        let word = state.hold;

        // L598-L600: the same value into both the resume counter and the caller's
        // report.
        let extra_len = low_u32(word);
        state.length = extra_len;
        if let Some(sink) = state.head.as_mut() {
            sink.extra_len = extra_len;
        }

        // L601-L602.
        if header_crc_enabled(state) {
            state.check = crc2(state.check, word);
        }

        // L603.
        state.init_bits();
    } else if let Some(sink) = state.head.as_mut() {
        // L605-L606: `state->head->extra = Z_NULL`.
        sink.clear_extra();
    }

    // L607-L608.
    state.mode = Mode::Extra;
    HeaderExit::advance()
}

// -----------------------------------------------------------------------------
//  `EXTRA` -- `inflate.c` L609-L629
// -----------------------------------------------------------------------------

/// Copies the gzip extra field into the caller's buffer, clamped, resumable.
///
/// The port of the `EXTRA` arm (`inflate.c` L609-L629), which is the most
/// security-sensitive state in the header:
///
/// ```c
/// if (state->flags & 0x0400) {
///     copy = state->length;
///     if (copy > have) copy = have;
///     if (copy) {
///         if (state->head != Z_NULL &&
///             state->head->extra != Z_NULL &&
///             (len = state->head->extra_len - state->length) <
///                 state->head->extra_max) {
///             zmemcpy(state->head->extra + len, next,
///                     len + copy > state->head->extra_max ?
///                     state->head->extra_max - len : copy);
///         }
///         if ((state->flags & 0x0200) && (state->wrap & 4))
///             state->check = crc32(state->check, next, copy);
///         have -= copy;
///         next += copy;
///         state->length -= copy;
///     }
///     if (state->length) goto inf_leave;
/// }
/// state->length = 0;
/// state->mode = NAME;
/// ```
///
/// # ★ The three guards and the clamp
///
/// All four conditions are reproduced, and none may be simplified away:
///
/// 1. a header must be installed -- otherwise there is nowhere to copy to, but the
///    input must still be consumed and the CRC must still advance;
/// 2. its `extra` pointer must be non-null -- the caller may want the length
///    without the bytes;
/// 3. the offset already reached, `extra_len - length`, must still be below
///    `extra_max`; and
/// 4. the copy length is `min(copy, extra_max - offset)`.
///
/// Guard 3 is what makes guard 4's subtraction non-negative, which is why they
/// cannot be reordered. In this port both are enforced by
/// [`GzHeaderSink::write_extra`], where the destination slice's length *is*
/// `extra_max`, so the clamp is a slice bound rather than an arithmetic promise.
///
/// # ★ Two things the offset arithmetic must get right
///
/// `extra_len - length` is unsigned subtraction in C and is therefore *meant* to
/// wrap: if a header is installed part-way through the field, `extra_len` is still
/// zero while `length` is not, and the wrapped result is a very large number that
/// fails guard 3 and copies nothing. `wrapping_sub` reproduces that exactly, where
/// a checked subtraction would have to invent a behaviour.
///
/// The CRC covers `copy` bytes -- every byte consumed from the input -- and not
/// the possibly shorter run that was stored. Conflating the two is invisible until
/// a gzip member with a header CRC and an oversized extra field arrives, at which
/// point every such member is rejected.
pub(crate) fn extra<'a, A: Allocator<'a>>(
    state: &mut InflateState<'a, A>,
    input: &[u8],
    next_in: &mut usize,
) -> HeaderExit {
    // L610: `if (state->flags & 0x0400)`.
    if (state.flags & FEXTRA) != 0 {
        // L611-L612: `copy = state->length; if (copy > have) copy = have;`.
        let available = input.len().saturating_sub(*next_in);
        let outstanding = state.length;
        let count = usize::try_from(outstanding)
            .unwrap_or(usize::MAX)
            .min(available);

        // L613: `if (copy)`.
        if count != 0 {
            // The bytes C addresses as `next[0..copy]`. `count <= available`, so
            // this always yields a slice; returning `NeedInput` if it somehow did
            // not is a no-op that leaves every cursor untouched.
            let Some(chunk) = input.get(*next_in..).and_then(|tail| tail.get(..count)) else {
                return HeaderExit::need_input();
            };

            // L614-L621: the three guards and the clamped copy. C discards the
            // number of bytes actually stored and so does this call: the cursors
            // below advance by the full `count` either way, because the CRC and
            // the input must move over bytes the buffer had no room for.
            if let Some(sink) = state.head.as_mut() {
                let offset = sink.extra_len.wrapping_sub(outstanding);
                sink.write_extra(offset, chunk);
            }

            // L622-L623: over every consumed byte, never over the stored run.
            if header_crc_enabled(state) {
                state.check = crc32(state.check, chunk);
            }

            // L624-L626: `have -= copy; next += copy; state->length -= copy;`.
            // `have` is not stored in this port -- it is derived from the slice and
            // the cursor -- so advancing the cursor is the whole update.
            *next_in = next_in.saturating_add(count);
            state.length = outstanding.saturating_sub(u32::try_from(count).unwrap_or(u32::MAX));
        }

        // L628: `if (state->length) goto inf_leave;` -- resumable mid-field.
        if state.length != 0 {
            return HeaderExit::need_input();
        }
    }

    // L630-L632: `state->length = 0` resets the counter for [`name`], which reuses
    // it as a write cursor rather than as a countdown.
    state.length = 0;
    state.mode = Mode::Name;
    HeaderExit::advance()
}

// -----------------------------------------------------------------------------
//  `NAME` and `COMMENT` -- `inflate.c` L633-L651 and L655-L673
// -----------------------------------------------------------------------------

/// Which of the two zero-terminated gzip strings a pass of
/// [`copy_string_field`] is reading.
///
/// The two C arms are character-for-character identical apart from the flag bit,
/// the destination field and the successor state, so they share one
/// implementation here and this selects between them. Sharing is not a
/// simplification: it makes it structurally impossible for the two to drift, which
/// in C is guaranteed only by the two copies having been kept in step by hand.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StringField {
    /// The `NAME` state's `head->name` / `name_max` pair (`inflate.c` L639-L642).
    Name,
    /// The `COMMENT` state's `head->comment` / `comm_max` pair
    /// (`inflate.c` L661-L664).
    Comment,
}

/// Consumes as much of a zero-terminated header string as the input holds.
///
/// The shared body of the `NAME` and `COMMENT` arms (`inflate.c` L634-L648 and
/// L656-L670):
///
/// ```c
/// if (have == 0) goto inf_leave;
/// copy = 0;
/// do {
///     len = (unsigned)(next[copy++]);
///     if (state->head != Z_NULL &&
///             state->head->name != Z_NULL &&
///             state->length < state->head->name_max)
///         state->head->name[state->length++] = (Bytef)len;
/// } while (len && copy < have);
/// if ((state->flags & 0x0200) && (state->wrap & 4))
///     state->check = crc32(state->check, next, copy);
/// have -= copy;
/// next += copy;
/// if (len) goto inf_leave;
/// ```
///
/// Returns `true` when the terminating zero was consumed, and `false` for C's two
/// `goto inf_leave` exits -- no input at all, or input exhausted before the zero.
/// Either way the bytes examined have been consumed and folded into the header
/// CRC, and `state.length` records how many were stored, so the next call resumes
/// exactly where this one stopped.
///
/// # ★ Three details of the loop
///
/// * It is a `do`/`while`, so it always consumes at least one byte. The `have == 0`
///   test above it is what makes that safe, and it is why this returns `false`
///   immediately rather than entering the loop on an empty chunk.
/// * **The terminating zero is stored like any other byte**, because the loop
///   exits *after* the store. So a caller's buffer is zero-terminated only when the
///   string fitted; when it was truncated there is no terminator, and this port
///   must not helpfully add one. `zlib.h` L126 describes `name` as pointing to a
///   "zero-terminated file name", and that promise is conditional on `name_max`
///   being large enough -- which is precisely why `name_max` exists.
/// * `state->length++` sits *inside* the guard, so the write cursor advances only
///   when a byte was really stored. [`GzHeaderSink::push_name`] reports that back
///   so the cursor can be advanced on exactly the same condition.
fn copy_string_field<'a, A: Allocator<'a>>(
    state: &mut InflateState<'a, A>,
    input: &[u8],
    next_in: &mut usize,
    field: StringField,
) -> bool {
    // L635 / L657: `if (have == 0) goto inf_leave;`.
    let available = input.len().saturating_sub(*next_in);
    if available == 0 {
        return false;
    }

    // L636-L643 / L658-L665: the `do`/`while` loop. `consumed` is C's `copy` and
    // `last` is C's `len`, the most recent byte, which decides both loop exits.
    let mut consumed = 0_usize;
    let mut last = 0_u8;
    while consumed < available {
        let Some(byte) = input.get(next_in.saturating_add(consumed)).copied() else {
            break;
        };
        consumed = consumed.saturating_add(1);
        last = byte;

        let at = state.length;
        let stored = state.head.as_mut().is_some_and(|sink| match field {
            StringField::Name => sink.push_name(at, byte),
            StringField::Comment => sink.push_comment(at, byte),
        });
        if stored {
            state.length = state.length.saturating_add(1);
        }

        // The `len` half of `while (len && copy < have)`; the `copy < have` half is
        // this loop's own condition.
        if byte == 0 {
            break;
        }
    }

    // L644-L645 / L666-L667: the CRC covers every byte examined, including the
    // terminating zero and including bytes that did not fit in the caller's buffer.
    if header_crc_enabled(state) {
        let chunk = input.get(*next_in..).and_then(|tail| tail.get(..consumed));
        if let Some(bytes) = chunk {
            state.check = crc32(state.check, bytes);
        }
    }

    // L646-L647 / L668-L669: `have -= copy; next += copy;`.
    *next_in = next_in.saturating_add(consumed);

    // L648 / L670: `if (len) goto inf_leave;`.
    last == 0
}

/// Reads the gzip file name, or records that there is none.
///
/// The port of the `NAME` arm (`inflate.c` L633-L651). See [`copy_string_field`]
/// for the loop, and note the `FNAME`-clear branch at L650-L651, which clears the
/// caller's `name` pointer to `Z_NULL` rather than leaving it alone.
///
/// `state.length` is reset on the way out (L652) so that [`comment`] starts its own
/// write cursor at zero.
pub(crate) fn name<'a, A: Allocator<'a>>(
    state: &mut InflateState<'a, A>,
    input: &[u8],
    next_in: &mut usize,
) -> HeaderExit {
    // L634: `if (state->flags & 0x0800)`.
    if (state.flags & FNAME) != 0 {
        let complete = copy_string_field(state, input, next_in, StringField::Name);
        if !complete {
            return HeaderExit::need_input();
        }
    } else if let Some(sink) = state.head.as_mut() {
        // L650-L651: `state->head->name = Z_NULL`.
        sink.clear_name();
    }

    // L652-L654.
    state.length = 0;
    state.mode = Mode::Comment;
    HeaderExit::advance()
}

/// Reads the gzip comment, or records that there is none.
///
/// The port of the `COMMENT` arm (`inflate.c` L655-L673): the same shape as
/// [`name`] with `FCOMMENT` in place of `FNAME` and `comm_max` in place of
/// `name_max`.
///
/// ★ One asymmetry with [`name`] is deliberate and is C's, not this port's: the
/// `NAME` arm resets `state->length` on the way out (L652) and this one does not
/// (L674), because the next state, `HCRC`, has no use for it. Reproducing the
/// asymmetry costs nothing and keeps the two files diffable.
pub(crate) fn comment<'a, A: Allocator<'a>>(
    state: &mut InflateState<'a, A>,
    input: &[u8],
    next_in: &mut usize,
) -> HeaderExit {
    // L656: `if (state->flags & 0x1000)`.
    if (state.flags & FCOMMENT) != 0 {
        let complete = copy_string_field(state, input, next_in, StringField::Comment);
        if !complete {
            return HeaderExit::need_input();
        }
    } else if let Some(sink) = state.head.as_mut() {
        // L672-L673: `state->head->comment = Z_NULL`.
        sink.clear_comment();
    }

    // L674-L675.
    state.mode = Mode::HCrc;
    HeaderExit::advance()
}

// -----------------------------------------------------------------------------
//  `HCRC` -- `inflate.c` L676-L692
// -----------------------------------------------------------------------------

/// Verifies the gzip header CRC-16, completes the header, and arms the body check.
///
/// The port of the `HCRC` arm (`inflate.c` L676-L692). Three things happen, and the
/// last is easy to miss:
///
/// * if `FHCRC` was set, the two-byte CRC-16 is compared against the low sixteen
///   bits of the header CRC accumulated by every state since [`head`] -- but only
///   when `wrap & 4` says the caller wants check values validated (L679);
/// * `hcrc` and `done = 1` are reported to the caller (L686-L689), which is how a
///   caller learns the header is complete;
/// * `state.check` is **reset** (L690), because a gzip trailer's CRC-32 covers the
///   *uncompressed data*, not the header (RFC 1952 §2.3.1). Reusing the header CRC
///   as the body's starting value would fail every well-formed member.
///
/// This is the state that hands off to [`Mode::Type`], where the DEFLATE data
/// itself begins.
pub(crate) fn hcrc<'a, A: Allocator<'a>>(
    state: &mut InflateState<'a, A>,
    input: &[u8],
    next_in: &mut usize,
) -> HeaderExit {
    // L677: `if (state->flags & 0x0200)`.
    if (state.flags & FHCRC) != 0 {
        // L678: `NEEDBITS(16)`.
        if !state.need_bits(FIELD_BITS_16, input, next_in) {
            return HeaderExit::need_input();
        }

        // L679-L683. The comparison is against the whole accumulator, as in C, so a
        // caller who primed extra bits in gets a mismatch rather than a match on a
        // truncated value.
        if state.wrap.verifies_check_value() && state.hold != u64::from(state.check & CRC16_MASK) {
            return reject(state, MSG_HEADER_CRC_MISMATCH);
        }

        // L684.
        state.init_bits();
    }

    // L686-L689: `head->hcrc = (state->flags >> 9) & 1` -- bit 9 of the word is
    // `FHCRC`, bit 1 of `FLG` -- and `head->done = 1`.
    let hcrc = ((state.flags >> 9) & 1) != 0;
    if let Some(sink) = state.head.as_mut() {
        sink.hcrc = hcrc;
        sink.mark_complete();
    }

    // L690: `strm->adler = state->check = crc32(0L, Z_NULL, 0)`, which is 0. An
    // empty slice gives the same 0 here, so unlike the Adler-32 initial value this
    // call is exact.
    state.check = crc32(0, &[]);

    // L691-L692.
    state.mode = Mode::Type;
    HeaderExit::advance_with_adler(state.check)
}

// -----------------------------------------------------------------------------
//  `DICTID` and `DICT` -- `inflate.c` L694-L706
// -----------------------------------------------------------------------------

/// Reads the four-byte Adler-32 of the preset dictionary a zlib stream requires.
///
/// The port of the `DICTID` arm (`inflate.c` L694-L698). Reached only when `FDICT`
/// was set in the zlib header (RFC 1950 §2.2), and the second of the two
/// `NEEDBITS(32)` sites.
///
/// ★ `ZSWAP32` (`zutil.h` L258-L259) is a byte reversal, and it is needed because
/// the two representations disagree: `DICTID` is stored most significant byte
/// first, as RFC 1950 §2.2 specifies for the `DICT` field, whereas the accumulator
/// filled from the low end. [`u32::swap_bytes`] is that reversal, and it is exact
/// for every input rather than only for the four terms C spells out.
///
/// The value is published in `strm->adler` (L696), which is the whole point of the
/// state: a caller that gets `Z_NEED_DICT` from the next state reads it to find out
/// *which* dictionary to supply (`zlib.h` L903-L906).
pub(crate) fn dict_id<'a, A: Allocator<'a>>(
    state: &mut InflateState<'a, A>,
    input: &[u8],
    next_in: &mut usize,
) -> HeaderExit {
    // L695: `NEEDBITS(32)`.
    if !state.need_bits(FIELD_BITS_32, input, next_in) {
        return HeaderExit::need_input();
    }

    // L696: `strm->adler = state->check = ZSWAP32(hold)`.
    state.check = low_u32(state.hold).swap_bytes();

    // L697-L699.
    state.init_bits();
    state.mode = Mode::Dict;
    HeaderExit::advance_with_adler(state.check)
}

/// Waits for `inflateSetDictionary`, then arms the zlib check value.
///
/// The port of the `DICT` arm (`inflate.c` L700-L706). This is the one state in the
/// header that consumes no input at all: it waits on the *caller*, which is why
/// [`HeaderAction::NeedDict`] is a distinct outcome rather than a kind of error.
///
/// ★ Returning `Z_NEED_DICT` without consuming further input is a contract, not an
/// implementation detail. C reaches it through `RESTORE(); return Z_NEED_DICT;`,
/// which deliberately skips the `inf_leave` epilogue, so the dictionary id
/// [`dict_id`] published in `strm->adler` survives for the caller to read and
/// `inflateSetDictionary` can validate the dictionary it is then handed
/// (`inflate.c` L1200-L1205). `test/example.c` exercises exactly this round trip,
/// and `test/infcover.c` L321-L334 walks the failure branches of it.
///
/// Once the dictionary has been installed the check value restarts from the
/// Adler-32 initial value (L705), because the trailer's Adler-32 covers the
/// uncompressed data and not the dictionary.
///
/// Takes no input parameters, unlike every other state here, because it reads
/// none.
pub(crate) fn dict<'a, A: Allocator<'a>>(state: &mut InflateState<'a, A>) -> HeaderExit {
    // L701-L704: `if (state->havedict == 0) { RESTORE(); return Z_NEED_DICT; }`.
    if !state.havedict {
        return HeaderExit::need_dict();
    }

    // L705: `strm->adler = state->check = adler32(0L, Z_NULL, 0)`, which is 1 --
    // see the note in [`head`] on why this is a constant and not a call.
    state.check = ADLER32_INITIAL_VALUE;

    // L706-L707.
    state.mode = Mode::Type;
    HeaderExit::advance_with_adler(state.check)
}

// -----------------------------------------------------------------------------
//  `inflateGetHeader` -- `inflate.c` L1219-L1231
// -----------------------------------------------------------------------------

/// Installs a caller's `gz_header` so the states above fill it in.
///
/// The port of `inflateGetHeader` (`inflate.c` L1219-L1231, declared at
/// `zlib.h` L1070):
///
/// ```c
/// if (inflateStateCheck(strm)) return Z_STREAM_ERROR;
/// state = (struct inflate_state FAR *)strm->state;
/// if ((state->wrap & 2) == 0) return Z_STREAM_ERROR;
/// state->head = head;
/// head->done = 0;
/// return Z_OK;
/// ```
///
/// # The two guards
///
/// The first, `inflateStateCheck`, validates a caller-supplied `z_stream`: that it
/// is non-null, that its allocation hooks are set, and that the opaque state
/// pointer really addresses an inflate state (`inflate.c` L88-L98). None of that
/// can be asked of a `&mut InflateState`, which is a valid state by construction,
/// so the pointer half of the check stays in `crates/libz-rs-sys/src/inflate.rs`
/// where the pointer is; `state.rs` exposes the tag half as `is_live_mode_tag`.
///
/// ★ The second guard is reproduced here in full: `(wrap & 2) == 0` means the
/// stream was never configured to decode a gzip header, so asking for one is a
/// [`ReturnCode::STREAM_ERROR`] and **not** a silent no-op. A raw stream, or one
/// opened for zlib only, therefore cannot install a header -- which is what
/// `test/infcover.c` relies on when it installs a header only for `win == 47`
/// (its L302-L310).
///
/// # `done` on install
///
/// `head->done = 0` is assigned on every install, even for a sink whose `done`
/// already said something else, because that is what makes the tri-state usable: a
/// caller can install a header, run `inflate()`, and read `0`, `-1` or `1` to learn
/// "not started", "not a gzip stream" or "complete" (`zlib.h` L131-L132; see the
/// table in this module's documentation).
///
/// # The capacities travel with the sink
///
/// `extra_max`, `name_max` and `comm_max` are capacities the *caller* advertises
/// (`zlib.h` L125, L127, L129), and they arrive as the lengths of the three slices
/// inside `head`. They are never inferred from anything else, and no state above
/// can write past them.
pub fn inflate_get_header<'a, A: Allocator<'a>>(
    state: &mut InflateState<'a, A>,
    mut head: GzHeaderSink<'a>,
) -> ReturnCode {
    // L1225: `if ((state->wrap & 2) == 0) return Z_STREAM_ERROR;`.
    if !state.wrap.allows_gzip_header() {
        return ReturnCode::STREAM_ERROR;
    }

    // L1228-L1230: `state->head = head; head->done = 0; return Z_OK;`.
    head.done = GZ_HEADER_PENDING;
    state.set_header_sink(Some(head));
    ReturnCode::OK
}

#[cfg(test)]
// The workspace denies the panic family and slice indexing in library code, which
// is exactly the property this module exists to guarantee; a test that cannot
// assert is useless, so the harness opts back in here only. Every index below is a
// literal or is bounded by the buffer it came from.
#[allow(
    clippy::indexing_slicing,
    clippy::panic,
    clippy::too_many_lines,
    clippy::unwrap_used
)]
mod tests {
    use super::{
        comment, crc2, crc4, dict, dict_id, ex_len, extra, flags, hcrc, head, inflate_get_header,
        name, os, time, HeaderAction, HeaderExit, MSG_HEADER_CRC_MISMATCH,
        MSG_INCORRECT_HEADER_CHECK, MSG_INVALID_WINDOW_SIZE, MSG_UNKNOWN_COMPRESSION_METHOD,
        MSG_UNKNOWN_HEADER_FLAGS_SET,
    };

    use alloc::vec;
    use alloc::vec::Vec;
    use core::cell::Cell;

    use crate::allocate::GlobalAllocator;
    use crate::config::InflateConfig;
    use crate::crc32::crc32;
    use crate::error::ReturnCode;
    use crate::inflate::mode::Mode;
    use crate::inflate::state::{
        GzHeaderSink, InflateState, FLAGS_NO_HEADER, GZ_HEADER_ABSENT, GZ_HEADER_COMPLETE,
        GZ_HEADER_PENDING,
    };

    /// The state type every test drives.
    type TestState<'a> = InflateState<'a, GlobalAllocator>;

    /// `windowBits` for a raw stream: `wrap == 0`.
    const RAW: i32 = -15;
    /// `windowBits` for a zlib-only stream with the maximum window.
    const ZLIB: i32 = 15;
    /// `windowBits` for a gzip-only stream: `15 + 16`.
    const GZIP: i32 = 31;
    /// `windowBits` for automatic detection: `15 + 32`, which `test/infcover.c`
    /// uses whenever it installs a header (its L302).
    const AUTO: i32 = 47;
    /// `windowBits` for automatic detection with the window size taken from the
    /// stream: `32`, whose low nibble is zero (`inflate.c` L154-L155).
    const AUTO_FROM_HEADER: i32 = 32;

    /// A fresh state for a `windowBits` request.
    fn state_for<'a>(window_bits: i32) -> TestState<'a> {
        InflateState::new(InflateConfig::new(window_bits), GlobalAllocator).unwrap()
    }

    /// Space-separated hex bytes, the notation `test/infcover.c` writes its test
    /// vectors in (its `h2b`, L206-L226).
    fn hex(text: &str) -> Vec<u8> {
        text.split_whitespace()
            .map(|token| u8::from_str_radix(token, 16).unwrap())
            .collect()
    }

    /// A caller buffer, pre-filled with a sentinel so that an out-of-bounds write
    /// would be visible rather than merely absent.
    ///
    /// `0xa5` is the byte `test/infcover.c`'s instrumented allocator fills with
    /// (its L108), chosen for the same reason.
    fn buffer(len: usize) -> Vec<Cell<u8>> {
        vec![Cell::new(0xa5); len]
    }

    /// The bytes currently in a caller buffer.
    fn contents(buffer: &[Cell<u8>]) -> Vec<u8> {
        buffer.iter().map(Cell::get).collect()
    }

    /// What the driver would report for a run of header states.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Outcome {
        /// The header is complete: [`Mode::Type`] or [`Mode::TypeDo`] was reached.
        Header,
        /// More input is needed; the state is unchanged and resumable.
        NeedInput,
        /// The header was rejected with this message.
        Bad(&'static str),
        /// A preset dictionary is required.
        NeedDict,
    }

    /// The header half of `inflate()`'s dispatch loop.
    ///
    /// This is deliberately the shape `crates/zlib-rs/src/inflate/mod.rs` will
    /// have, so the tests exercise the real contract -- including the fact that
    /// [`HeaderAction::Advance`] must always make progress, since a handler that
    /// returned it without advancing the mode would hang here rather than fail an
    /// assertion.
    fn drive(
        state: &mut TestState<'_>,
        input: &[u8],
        next_in: &mut usize,
    ) -> (Outcome, Option<u32>) {
        let mut adler = None;
        loop {
            let exit = match state.mode {
                Mode::Head => head(state, input, next_in),
                Mode::Flags => flags(state, input, next_in),
                Mode::Time => time(state, input, next_in),
                Mode::Os => os(state, input, next_in),
                Mode::ExLen => ex_len(state, input, next_in),
                Mode::Extra => extra(state, input, next_in),
                Mode::Name => name(state, input, next_in),
                Mode::Comment => comment(state, input, next_in),
                Mode::HCrc => hcrc(state, input, next_in),
                Mode::DictId => dict_id(state, input, next_in),
                Mode::Dict => dict(state),
                _ => return (Outcome::Header, adler),
            };
            if let Some(value) = exit.adler {
                adler = Some(value);
            }
            match exit.action {
                HeaderAction::Advance => {}
                HeaderAction::NeedInput => return (Outcome::NeedInput, adler),
                HeaderAction::Bad => {
                    assert_eq!(state.mode, Mode::Bad, "a rejection must enter Mode::Bad");
                    return (Outcome::Bad(exit.msg.unwrap()), adler);
                }
                HeaderAction::NeedDict => {
                    assert_eq!(
                        state.mode,
                        Mode::Dict,
                        "Z_NEED_DICT must stay in Mode::Dict"
                    );
                    return (Outcome::NeedDict, adler);
                }
            }
        }
    }

    /// Drives a header while revealing only `step` more bytes of input per pass.
    ///
    /// This is what a caller feeding `inflate()` in small chunks does: `avail_in`
    /// grows, `next_in` persists, and the state machine must resume where it
    /// stopped. `step == input.len()` is the single-shot case.
    fn drive_chunked(
        state: &mut TestState<'_>,
        input: &[u8],
        step: usize,
    ) -> (Outcome, Option<u32>, usize) {
        let mut next_in = 0;
        let mut adler = None;
        let mut visible = 0_usize;
        loop {
            visible = visible.saturating_add(step).min(input.len());
            let (outcome, published) = drive(state, &input[..visible], &mut next_in);
            if let Some(value) = published {
                adler = Some(value);
            }
            if outcome == Outcome::NeedInput && visible < input.len() {
                continue;
            }
            return (outcome, adler, next_in);
        }
    }

    /// The two bytes of a zlib header for an explicit `CMF`, with `FCHECK` chosen
    /// so that `CMF * 256 + FLG` is a multiple of 31 (RFC 1950 §2.2).
    fn zlib_header_from_cmf(cmf: u8, fdict: bool) -> [u8; 2] {
        let base = if fdict { 0x20 } else { 0x00 };
        let value = (u32::from(cmf) << 8) + u32::from(base);
        let remainder = value % 31;
        let flg = if remainder == 0 {
            base
        } else {
            base + u8::try_from(31 - remainder).unwrap()
        };
        [cmf, flg]
    }

    /// The two bytes of a valid zlib header for a window exponent.
    fn zlib_header(window_bits: u32, fdict: bool) -> [u8; 2] {
        let cinfo = window_bits - 8;
        let cmf = u8::try_from(0x08 | (cinfo << 4)).unwrap();
        zlib_header_from_cmf(cmf, fdict)
    }

    /// A gzip member header, assembled the way RFC 1952 §2.3.1 lays it out.
    #[derive(Debug, Clone, Copy)]
    struct Gzip<'a> {
        /// `CM`, normally `Z_DEFLATED`.
        method: u8,
        /// `FTEXT`, bit 0 of `FLG`.
        text: bool,
        /// `FHCRC`, bit 1 of `FLG`: append a header CRC-16.
        hcrc: bool,
        /// `FEXTRA` payload, if any.
        extra: Option<&'a [u8]>,
        /// `FNAME` payload, if any; the terminating zero is added here.
        name: Option<&'a [u8]>,
        /// `FCOMMENT` payload, if any; the terminating zero is added here.
        comment: Option<&'a [u8]>,
        /// Extra `FLG` bits to force on, for the reserved-bit cases.
        reserved: u8,
        /// `MTIME`.
        mtime: u32,
        /// `XFL`.
        xfl: u8,
        /// `OS`.
        operating_system: u8,
        /// Store a deliberately wrong header CRC-16.
        corrupt_hcrc: bool,
    }

    impl Gzip<'_> {
        /// A minimal well-formed header: deflate method, no optional fields.
        fn new() -> Self {
            Self {
                method: 8,
                text: false,
                hcrc: false,
                extra: None,
                name: None,
                comment: None,
                reserved: 0,
                mtime: 0,
                xfl: 0,
                operating_system: 0,
                corrupt_hcrc: false,
            }
        }

        /// The `FLG` byte this header describes.
        fn flg(self) -> u8 {
            let mut flg = self.reserved;
            if self.text {
                flg |= 0x01;
            }
            if self.hcrc {
                flg |= 0x02;
            }
            if self.extra.is_some() {
                flg |= 0x04;
            }
            if self.name.is_some() {
                flg |= 0x08;
            }
            if self.comment.is_some() {
                flg |= 0x10;
            }
            flg
        }

        /// The header bytes, including the CRC-16 when `FHCRC` is set.
        fn bytes(self) -> Vec<u8> {
            let mut out = vec![0x1f, 0x8b, self.method, self.flg()];
            out.extend_from_slice(&self.mtime.to_le_bytes());
            out.push(self.xfl);
            out.push(self.operating_system);
            if let Some(field) = self.extra {
                let len = u16::try_from(field.len()).unwrap();
                out.extend_from_slice(&len.to_le_bytes());
                out.extend_from_slice(field);
            }
            if let Some(field) = self.name {
                out.extend_from_slice(field);
                out.push(0);
            }
            if let Some(field) = self.comment {
                out.extend_from_slice(field);
                out.push(0);
            }
            if self.hcrc {
                let mut check = u16::try_from(crc32(0, &out) & 0xffff).unwrap();
                if self.corrupt_hcrc {
                    check ^= 0xffff;
                }
                out.extend_from_slice(&check.to_le_bytes());
            }
            out
        }
    }

    // -------------------------------------------------------------------------
    //  Raw streams
    // -------------------------------------------------------------------------

    /// `inflate.c` L507-L510: a raw stream has no header, so `HEAD` reaches
    /// `TYPEDO` without looking at a single byte.
    #[test]
    fn raw_stream_skips_the_header() {
        let mut state = state_for(RAW);
        let mut next_in = 0;
        let exit = head(&mut state, &[], &mut next_in);

        assert_eq!(exit.action, HeaderAction::Advance);
        assert_eq!(exit.adler, None, "a raw stream publishes no check value");
        assert_eq!(exit.msg, None);
        assert_eq!(state.mode, Mode::TypeDo);
        assert_eq!(next_in, 0, "no input may be consumed");
        assert_eq!(
            state.flags, FLAGS_NO_HEADER,
            "`flags` stays -1 for raw data"
        );
    }

    // -------------------------------------------------------------------------
    //  zlib headers
    // -------------------------------------------------------------------------

    /// A valid zlib header is accepted for every window size the format allows,
    /// and leaves exactly the state `inflate.c` L547-L552 describes.
    #[test]
    fn zlib_header_is_accepted_for_every_window_size() {
        for window_bits in 8..=15_u32 {
            let mut state = state_for(i32::try_from(window_bits).unwrap());
            let header = zlib_header(window_bits, false);
            let mut next_in = 0;
            let (outcome, adler) = drive(&mut state, &header, &mut next_in);

            assert_eq!(outcome, Outcome::Header, "windowBits {window_bits}");
            assert_eq!(state.mode, Mode::Type);
            assert_eq!(next_in, 2, "both header bytes are consumed");
            assert_eq!(state.flags, 0, "`flags == 0` marks a zlib header");
            assert_eq!(state.dmax, 1 << window_bits);
            assert_eq!(state.check, 1, "the zlib check value starts at 1");
            assert_eq!(
                adler,
                Some(1),
                "adler32(0, Z_NULL, 0) is 1, not 0 -- inflate.c L550"
            );
        }
    }

    /// The `FDICT` bit sends the stream to `DICTID` (`inflate.c` L551), and it is
    /// read *after* `DROPBITS(4)`, which is why the reference tests `0x200`.
    #[test]
    fn zlib_header_with_fdict_goes_to_the_dictionary_id() {
        let mut state = state_for(ZLIB);
        let header = zlib_header(15, true);
        let mut next_in = 0;
        let exit = head(&mut state, &header, &mut next_in);

        assert_eq!(exit.action, HeaderAction::Advance);
        assert_eq!(state.mode, Mode::DictId);
        assert_eq!(exit.adler, Some(1));
    }

    /// Every one of the 65 536 possible two-byte zlib headers is classified
    /// exactly as `inflate.c` L524-L552 classifies it.
    ///
    /// This is the cheapest complete statement of the acceptance rule: the check
    /// value, the method, the window bound and the `FDICT` dispatch, over the
    /// whole input space rather than over chosen examples.
    #[test]
    fn every_two_byte_zlib_header_is_classified_like_the_reference() {
        let mut state = state_for(ZLIB);
        for cmf in 0..=0xff_u32 {
            for flg in 0..=0xff_u32 {
                let _ = state.reset();
                let header = [u8::try_from(cmf).unwrap(), u8::try_from(flg).unwrap()];
                let mut next_in = 0;
                let exit = head(&mut state, &header, &mut next_in);

                let value = (cmf << 8) + flg;
                let len = (cmf >> 4) + 8;
                let (expected_action, expected_msg, expected_mode) = if value % 31 != 0 {
                    (
                        HeaderAction::Bad,
                        Some(MSG_INCORRECT_HEADER_CHECK),
                        Mode::Bad,
                    )
                } else if cmf & 0xf != 8 {
                    (
                        HeaderAction::Bad,
                        Some(MSG_UNKNOWN_COMPRESSION_METHOD),
                        Mode::Bad,
                    )
                } else if len > 15 {
                    (HeaderAction::Bad, Some(MSG_INVALID_WINDOW_SIZE), Mode::Bad)
                } else if flg & 0x20 != 0 {
                    (HeaderAction::Advance, None, Mode::DictId)
                } else {
                    (HeaderAction::Advance, None, Mode::Type)
                };

                assert_eq!(
                    exit.action, expected_action,
                    "cmf {cmf:#04x} flg {flg:#04x}"
                );
                assert_eq!(exit.msg, expected_msg, "cmf {cmf:#04x} flg {flg:#04x}");
                assert_eq!(state.mode, expected_mode, "cmf {cmf:#04x} flg {flg:#04x}");
                assert_eq!(next_in, 2, "both bytes are always consumed");
            }
        }
    }

    /// `test/infcover.c` L401: `77 85` passes the check value but names method 7.
    ///
    /// The message is the same as the gzip path's, so the test also pins *which*
    /// site produced it: the zlib path rejects before `flags` is assigned, so
    /// `flags` is still `-1`.
    #[test]
    fn zlib_method_other_than_deflate_is_rejected() {
        let mut state = state_for(ZLIB);
        let input = hex("77 85");
        let mut next_in = 0;
        let (outcome, adler) = drive(&mut state, &input, &mut next_in);

        assert_eq!(outcome, Outcome::Bad(MSG_UNKNOWN_COMPRESSION_METHOD));
        assert_eq!(adler, None);
        assert_eq!(
            state.flags, FLAGS_NO_HEADER,
            "the zlib path rejects before `flags` is set -- inflate.c L533"
        );
    }

    /// `test/infcover.c` L409: `78 90` is a well-formed method with a bad check.
    #[test]
    fn zlib_header_check_is_enforced() {
        let mut state = state_for(AUTO);
        let input = hex("78 90");
        let mut next_in = 0;
        let (outcome, _) = drive(&mut state, &input, &mut next_in);

        assert_eq!(outcome, Outcome::Bad(MSG_INCORRECT_HEADER_CHECK));
    }

    /// A zlib header is refused outright when bit 0 of `wrap` is clear, before the
    /// check value is even considered (`inflate.c` L524).
    #[test]
    fn zlib_header_is_refused_on_a_gzip_only_stream() {
        let mut state = state_for(GZIP);
        let header = zlib_header(15, false);
        let mut next_in = 0;
        let (outcome, _) = drive(&mut state, &header, &mut next_in);

        assert_eq!(outcome, Outcome::Bad(MSG_INCORRECT_HEADER_CHECK));
    }

    /// `test/infcover.c` L403: `78 9c` needs a 32 KiB window, but the caller asked
    /// for 256 bytes.
    #[test]
    fn zlib_window_larger_than_requested_is_rejected() {
        let mut state = state_for(8);
        let input = hex("78 9c");
        let mut next_in = 0;
        let (outcome, _) = drive(&mut state, &input, &mut next_in);

        assert_eq!(outcome, Outcome::Bad(MSG_INVALID_WINDOW_SIZE));
    }

    /// The other half of the same test: a `CINFO` above 7 exceeds `MAX_WBITS`
    /// however large a window the caller offered (`inflate.c` L542).
    #[test]
    fn zlib_window_larger_than_the_format_allows_is_rejected() {
        for cinfo in 8..=15_u32 {
            let mut state = state_for(ZLIB);
            let cmf = u8::try_from(0x08 | (cinfo << 4)).unwrap();
            let header = zlib_header_from_cmf(cmf, false);
            let mut next_in = 0;
            let (outcome, _) = drive(&mut state, &header, &mut next_in);

            assert_eq!(
                outcome,
                Outcome::Bad(MSG_INVALID_WINDOW_SIZE),
                "CINFO {cinfo} encodes a {} bit window",
                cinfo + 8
            );
        }
    }

    /// `test/infcover.c` L402: `windowBits == 0` means "take the window size from
    /// the header" (`inflate.c` L540-L541), and `8 99` names a 256-byte window.
    #[test]
    fn window_size_is_taken_from_the_zlib_header() {
        let mut state = state_for(0);
        assert_eq!(state.wbits, 0, "the request left the exponent unset");

        let input = hex("8 99");
        let mut next_in = 0;
        let (outcome, adler) = drive(&mut state, &input, &mut next_in);

        assert_eq!(outcome, Outcome::Header);
        assert_eq!(state.wbits, 8, "the exponent comes from CINFO + 8");
        assert_eq!(state.dmax, 1 << 8);
        assert_eq!(adler, Some(1));
    }

    /// The gzip counterpart: a member header carries no window field, so the
    /// maximum is assumed (`inflate.c` L514-L515).
    #[test]
    fn window_size_defaults_to_the_maximum_for_gzip() {
        let mut state = state_for(AUTO_FROM_HEADER);
        assert_eq!(state.wbits, 0);

        let input = Gzip::new().bytes();
        let mut next_in = 0;
        let (outcome, _) = drive(&mut state, &input, &mut next_in);

        assert_eq!(outcome, Outcome::Header);
        assert_eq!(state.wbits, 15);
    }

    // -------------------------------------------------------------------------
    //  gzip headers
    // -------------------------------------------------------------------------

    /// A minimal gzip header is accepted, and `HCRC` re-arms the check value for
    /// the body (`inflate.c` L690).
    #[test]
    fn minimal_gzip_header_is_accepted() {
        let mut state = state_for(GZIP);
        let input = Gzip::new().bytes();
        let mut next_in = 0;
        let (outcome, adler) = drive(&mut state, &input, &mut next_in);

        assert_eq!(outcome, Outcome::Header);
        assert_eq!(state.mode, Mode::Type);
        assert_eq!(next_in, 10, "a minimal gzip header is ten bytes");
        assert_ne!(
            state.flags, 0,
            "a non-zero `flags` is what selects CRC-32 for the body"
        );
        assert_eq!(state.check, 0, "the body CRC starts at 0");
        assert_eq!(adler, Some(0));
    }

    /// `test/infcover.c` L399: `1f 8b 0 0` names method 0.
    ///
    /// The message matches the zlib path's, so the test pins the site: the gzip
    /// path assigns `flags` *before* rejecting, so `flags` is the `CM`/`FLG` word.
    #[test]
    fn gzip_method_other_than_deflate_is_rejected() {
        let mut state = state_for(GZIP);
        let input = hex("1f 8b 7 0");
        let mut next_in = 0;
        let (outcome, _) = drive(&mut state, &input, &mut next_in);

        assert_eq!(outcome, Outcome::Bad(MSG_UNKNOWN_COMPRESSION_METHOD));
        assert_eq!(
            state.flags, 7,
            "the gzip path assigns `flags` first -- inflate.c L557"
        );

        // And the vector `test/infcover.c` actually uses.
        let mut state = state_for(GZIP);
        let input = hex("1f 8b 0 0");
        let mut next_in = 0;
        assert_eq!(
            drive(&mut state, &input, &mut next_in).0,
            Outcome::Bad(MSG_UNKNOWN_COMPRESSION_METHOD)
        );
    }

    /// `test/infcover.c` L400: `1f 8b 8 80` sets a reserved `FLG` bit.
    ///
    /// All three reserved bits are checked, because the mask is `0xe000` and a
    /// single example would not distinguish it from a narrower one.
    #[test]
    fn gzip_reserved_flag_bits_are_rejected() {
        for reserved in [0x20_u8, 0x40, 0x80] {
            let mut state = state_for(GZIP);
            let header = Gzip {
                reserved,
                ..Gzip::new()
            };
            let input = header.bytes();
            let mut next_in = 0;
            let (outcome, _) = drive(&mut state, &input, &mut next_in);

            assert_eq!(
                outcome,
                Outcome::Bad(MSG_UNKNOWN_HEADER_FLAGS_SET),
                "FLG bit {reserved:#04x}"
            );
        }

        let mut state = state_for(GZIP);
        let input = hex("1f 8b 8 80");
        let mut next_in = 0;
        assert_eq!(
            drive(&mut state, &input, &mut next_in).0,
            Outcome::Bad(MSG_UNKNOWN_HEADER_FLAGS_SET)
        );
    }

    /// Every combination of the five defined `FLG` bits is parsed, and each
    /// optional field lands where RFC 1952 §2.3.1 says it does.
    ///
    /// The absent fields are checked too: the reference does not merely skip a
    /// missing field, it clears the caller's pointer (`inflate.c` L605-L606,
    /// L650-L651, L672-L673).
    #[test]
    fn gzip_flag_matrix_is_parsed_field_for_field() {
        let extra_field = [0xde_u8, 0xad, 0xbe, 0xef];
        let file_name = b"archive.tar";
        let file_comment = b"a comment";

        for bits in 0..32_u8 {
            let text = bits & 1 != 0;
            let hcrc = bits & 2 != 0;
            let header = Gzip {
                text,
                hcrc,
                extra: if bits & 4 != 0 {
                    Some(&extra_field)
                } else {
                    None
                },
                name: if bits & 8 != 0 { Some(file_name) } else { None },
                comment: if bits & 16 != 0 {
                    Some(file_comment)
                } else {
                    None
                },
                mtime: 0x1234_5678,
                xfl: 2,
                operating_system: 3,
                ..Gzip::new()
            };
            let input = header.bytes();

            let extra_out = buffer(8);
            let name_out = buffer(32);
            let comment_out = buffer(32);
            let mut state = state_for(AUTO);
            assert_eq!(
                inflate_get_header(
                    &mut state,
                    GzHeaderSink::new(
                        Some(extra_out.as_slice()),
                        Some(name_out.as_slice()),
                        Some(comment_out.as_slice()),
                    ),
                ),
                ReturnCode::OK
            );

            let mut next_in = 0;
            let (outcome, adler) = drive(&mut state, &input, &mut next_in);
            assert_eq!(outcome, Outcome::Header, "FLG bits {bits:#04x}");
            assert_eq!(next_in, input.len(), "FLG bits {bits:#04x}");
            assert_eq!(adler, Some(0));

            let sink = state.head.unwrap();
            assert_eq!(sink.done, GZ_HEADER_COMPLETE, "FLG bits {bits:#04x}");
            assert_eq!(sink.text, text, "FLG bits {bits:#04x}");
            assert_eq!(sink.hcrc, hcrc, "FLG bits {bits:#04x}");
            assert_eq!(sink.time, 0x1234_5678);
            assert_eq!(sink.xflags, 2);
            assert_eq!(sink.os, 3);

            if bits & 4 != 0 {
                assert_eq!(sink.extra_len, 4, "FLG bits {bits:#04x}");
                assert!(!sink.extra_is_absent());
                assert_eq!(&contents(&extra_out)[..4], &extra_field);
            } else {
                assert!(
                    sink.extra_is_absent(),
                    "a missing extra field nulls the pointer"
                );
                assert_eq!(contents(&extra_out), vec![0xa5; 8]);
            }

            if bits & 8 != 0 {
                assert!(!sink.name_is_absent());
                assert_eq!(&contents(&name_out)[..file_name.len()], file_name);
                assert_eq!(
                    contents(&name_out)[file_name.len()],
                    0,
                    "a name that fits is zero-terminated"
                );
            } else {
                assert!(sink.name_is_absent());
                assert_eq!(contents(&name_out), vec![0xa5; 32]);
            }

            if bits & 16 != 0 {
                assert!(!sink.comment_is_absent());
                assert_eq!(&contents(&comment_out)[..file_comment.len()], file_comment);
                assert_eq!(contents(&comment_out)[file_comment.len()], 0);
            } else {
                assert!(sink.comment_is_absent());
                assert_eq!(contents(&comment_out), vec![0xa5; 32]);
            }
        }
    }

    /// A correct header CRC-16 is accepted, and the value is the one
    /// `test/infcover.c` L407 hard-codes: `1d 26` for a ten-byte header with
    /// `FLG == 0x02`.
    ///
    /// That constant is the strongest available check on [`crc2`] and [`crc4`]: it
    /// can only match if every header byte was folded in, in wire order, exactly
    /// once.
    #[test]
    fn gzip_header_crc_matches_the_reference_vector() {
        let input = hex("1f 8b 8 2 0 0 0 0 0 0 1d 26");
        let mut state = state_for(AUTO);
        let mut next_in = 0;
        let (outcome, _) = drive(&mut state, &input, &mut next_in);

        assert_eq!(outcome, Outcome::Header);
        assert_eq!(next_in, 12, "ten header bytes plus the CRC-16");

        // The same header, assembled rather than transcribed.
        let built = Gzip {
            hcrc: true,
            ..Gzip::new()
        }
        .bytes();
        assert_eq!(built, input);
    }

    /// `test/infcover.c` L405-L406: a header whose CRC-16 does not match is a data
    /// error (`inflate.c` L679-L683).
    #[test]
    fn gzip_header_crc_mismatch_is_rejected() {
        let input = hex("1f 8b 8 1e 0 0 0 0 0 0 1 0 0 0 0 0 0");
        let out = buffer(1);
        let mut state = state_for(AUTO);
        assert_eq!(
            inflate_get_header(
                &mut state,
                GzHeaderSink::new(
                    Some(out.as_slice()),
                    Some(out.as_slice()),
                    Some(out.as_slice()),
                ),
            ),
            ReturnCode::OK
        );
        let mut next_in = 0;
        let (outcome, _) = drive(&mut state, &input, &mut next_in);

        assert_eq!(outcome, Outcome::Bad(MSG_HEADER_CRC_MISMATCH));
        assert_eq!(
            state.head.unwrap().done,
            GZ_HEADER_PENDING,
            "a rejected header never reports itself complete"
        );

        // The assembled equivalent, for every optional field.
        let field = [1_u8, 2, 3];
        let mut state = state_for(GZIP);
        let corrupt = Gzip {
            hcrc: true,
            corrupt_hcrc: true,
            extra: Some(&field),
            name: Some(b"n"),
            comment: Some(b"c"),
            ..Gzip::new()
        }
        .bytes();
        let mut next_in = 0;
        assert_eq!(
            drive(&mut state, &corrupt, &mut next_in).0,
            Outcome::Bad(MSG_HEADER_CRC_MISMATCH)
        );
    }

    /// `inflateValidate(strm, 0)` clears `wrap & 4`, and then even a wrong header
    /// CRC is accepted (`inflate.c` L679, L1384-L1394).
    #[test]
    fn header_crc_is_not_checked_when_validation_is_disabled() {
        let mut state = state_for(GZIP);
        state.set_validate(false);
        let input = Gzip {
            hcrc: true,
            corrupt_hcrc: true,
            ..Gzip::new()
        }
        .bytes();
        let mut next_in = 0;
        let (outcome, _) = drive(&mut state, &input, &mut next_in);

        assert_eq!(outcome, Outcome::Header);
        assert_eq!(next_in, input.len());
    }

    // -------------------------------------------------------------------------
    //  The bounded writes
    // -------------------------------------------------------------------------

    /// ★ An extra field longer than `extra_max` stores exactly `extra_max` bytes,
    /// reports the true length, and folds *every* consumed byte into the header CRC
    /// (`inflate.c` L614-L623).
    ///
    /// The header CRC is what proves the last claim: `FHCRC` is set, so the run
    /// only completes if the CRC covered all eight extra bytes and not just the
    /// three that fitted.
    #[test]
    fn extra_field_is_clamped_to_extra_max() {
        let field = [1_u8, 2, 3, 4, 5, 6, 7, 8];
        let input = Gzip {
            hcrc: true,
            extra: Some(&field),
            ..Gzip::new()
        }
        .bytes();

        let extra_out = buffer(3);
        let mut state = state_for(GZIP);
        assert_eq!(
            inflate_get_header(
                &mut state,
                GzHeaderSink::new(Some(extra_out.as_slice()), None, None),
            ),
            ReturnCode::OK
        );
        let mut next_in = 0;
        let (outcome, _) = drive(&mut state, &input, &mut next_in);

        assert_eq!(
            outcome,
            Outcome::Header,
            "a passing header CRC proves every consumed byte was folded in"
        );
        assert_eq!(next_in, input.len());
        assert_eq!(
            contents(&extra_out),
            vec![1, 2, 3],
            "exactly extra_max bytes"
        );

        let sink = state.head.unwrap();
        assert_eq!(
            sink.extra_len, 8,
            "the advertised length, not the stored count"
        );
        assert_eq!(sink.extra_max(), 3);
        assert!(!sink.extra_is_absent());
    }

    /// The same field with `extra == Z_NULL`, and with no header installed at all:
    /// both must consume the input and compute the CRC without writing anything
    /// (`inflate.c` L614-L615).
    #[test]
    fn extra_field_without_a_buffer_is_still_consumed_and_checked() {
        let field = [9_u8; 40];
        let input = Gzip {
            hcrc: true,
            extra: Some(&field),
            ..Gzip::new()
        }
        .bytes();

        // `extra == Z_NULL`, but a header is installed, so the length is reported.
        let name_out = buffer(4);
        let mut state = state_for(GZIP);
        assert_eq!(
            inflate_get_header(
                &mut state,
                GzHeaderSink::new(None, Some(name_out.as_slice()), None),
            ),
            ReturnCode::OK
        );
        let mut next_in = 0;
        assert_eq!(drive(&mut state, &input, &mut next_in).0, Outcome::Header);
        assert_eq!(next_in, input.len());
        assert_eq!(state.head.unwrap().extra_len, 40);
        assert_eq!(
            contents(&name_out),
            vec![0xa5; 4],
            "nothing else was touched"
        );

        // No header at all: `state->head == Z_NULL`.
        let mut state = state_for(GZIP);
        let mut next_in = 0;
        assert_eq!(drive(&mut state, &input, &mut next_in).0, Outcome::Header);
        assert_eq!(next_in, input.len());
        assert!(state.head.is_none());
    }

    /// A zero-length extra field is legal: `XLEN == 0` advances straight to `NAME`
    /// without consuming a payload byte (`inflate.c` L610-L613).
    #[test]
    fn zero_length_extra_field_is_accepted() {
        let input = Gzip {
            extra: Some(&[]),
            ..Gzip::new()
        }
        .bytes();

        let extra_out = buffer(4);
        let mut state = state_for(GZIP);
        assert_eq!(
            inflate_get_header(
                &mut state,
                GzHeaderSink::new(Some(extra_out.as_slice()), None, None),
            ),
            ReturnCode::OK
        );
        let mut next_in = 0;
        assert_eq!(drive(&mut state, &input, &mut next_in).0, Outcome::Header);
        assert_eq!(next_in, 12, "ten header bytes plus a two-byte XLEN");

        let sink = state.head.unwrap();
        assert_eq!(sink.extra_len, 0);
        assert!(
            !sink.extra_is_absent(),
            "an empty field is present, not absent"
        );
        assert_eq!(contents(&extra_out), vec![0xa5; 4]);
    }

    /// ★ A name longer than `name_max` is truncated **without** a terminator, while
    /// one that fits keeps the zero the stream carried (`inflate.c` L639-L643).
    #[test]
    fn name_is_truncated_without_a_terminator() {
        // Truncated: three of the six bytes fit, and the zero never reaches the
        // buffer.
        let input = Gzip {
            hcrc: true,
            name: Some(b"abcdef"),
            ..Gzip::new()
        }
        .bytes();
        let name_out = buffer(3);
        let mut state = state_for(GZIP);
        assert_eq!(
            inflate_get_header(
                &mut state,
                GzHeaderSink::new(None, Some(name_out.as_slice()), None),
            ),
            ReturnCode::OK
        );
        let mut next_in = 0;
        assert_eq!(drive(&mut state, &input, &mut next_in).0, Outcome::Header);
        assert_eq!(next_in, input.len());
        assert_eq!(
            contents(&name_out),
            b"abc".to_vec(),
            "no terminator is added when the name did not fit"
        );

        // It fits: two bytes plus the zero.
        let input = Gzip {
            name: Some(b"ab"),
            ..Gzip::new()
        }
        .bytes();
        let name_out = buffer(4);
        let mut state = state_for(GZIP);
        assert_eq!(
            inflate_get_header(
                &mut state,
                GzHeaderSink::new(None, Some(name_out.as_slice()), None),
            ),
            ReturnCode::OK
        );
        let mut next_in = 0;
        assert_eq!(drive(&mut state, &input, &mut next_in).0, Outcome::Header);
        assert_eq!(contents(&name_out), vec![b'a', b'b', 0, 0xa5]);
    }

    /// The comment is clamped by `comm_max` the same way, and the two cursors are
    /// independent: a truncated name must not shorten the comment
    /// (`inflate.c` L652 resets `state->length` between them).
    #[test]
    fn comment_is_truncated_independently_of_the_name() {
        let input = Gzip {
            hcrc: true,
            name: Some(b"a-very-long-file-name"),
            comment: Some(b"comment"),
            ..Gzip::new()
        }
        .bytes();

        let name_out = buffer(2);
        let comment_out = buffer(4);
        let mut state = state_for(GZIP);
        assert_eq!(
            inflate_get_header(
                &mut state,
                GzHeaderSink::new(
                    None,
                    Some(name_out.as_slice()),
                    Some(comment_out.as_slice()),
                ),
            ),
            ReturnCode::OK
        );
        let mut next_in = 0;
        assert_eq!(drive(&mut state, &input, &mut next_in).0, Outcome::Header);
        assert_eq!(next_in, input.len());
        assert_eq!(contents(&name_out), b"a-".to_vec());
        assert_eq!(
            contents(&comment_out),
            b"comm".to_vec(),
            "the comment starts at its own offset zero"
        );
    }

    /// `test/infcover.c` L303-L308 points all three fields at one buffer, which is
    /// legal and must not corrupt anything: the last field written wins.
    #[test]
    fn aliased_caller_buffers_are_supported() {
        let field = [0x11_u8, 0x22];
        let input = Gzip {
            hcrc: true,
            extra: Some(&field),
            name: Some(b"nm"),
            comment: Some(b"cm"),
            ..Gzip::new()
        }
        .bytes();

        let shared = buffer(4);
        let mut state = state_for(AUTO);
        assert_eq!(
            inflate_get_header(
                &mut state,
                GzHeaderSink::new(
                    Some(shared.as_slice()),
                    Some(shared.as_slice()),
                    Some(shared.as_slice()),
                ),
            ),
            ReturnCode::OK
        );
        let mut next_in = 0;
        assert_eq!(drive(&mut state, &input, &mut next_in).0, Outcome::Header);
        assert_eq!(next_in, input.len());
        assert_eq!(
            contents(&shared),
            vec![b'c', b'm', 0, 0xa5],
            "the comment was written last"
        );
        assert_eq!(state.head.unwrap().done, GZ_HEADER_COMPLETE);
    }

    // -------------------------------------------------------------------------
    //  Resumability
    // -------------------------------------------------------------------------

    /// Everything a caller can observe after a header run.
    #[derive(Debug, Clone, PartialEq, Eq)]
    struct Snapshot {
        outcome: Outcome,
        adler: Option<u32>,
        next_in: usize,
        mode: Mode,
        check: u32,
        flags: i32,
        wbits: u32,
        done: i32,
        text: bool,
        time: u32,
        xflags: i32,
        os: i32,
        extra_len: u32,
        hcrc: bool,
        extra: Vec<u8>,
        name: Vec<u8>,
        comment: Vec<u8>,
    }

    /// Parses `input` while revealing `step` bytes at a time, and reports
    /// everything observable afterwards.
    ///
    /// A header sink is installed whenever the stream permits gzip decoding, which
    /// is the only configuration in which `inflateGetHeader` succeeds.
    fn parse(window_bits: i32, input: &[u8], step: usize) -> Snapshot {
        let extra_out = buffer(8);
        let name_out = buffer(24);
        let comment_out = buffer(24);
        let mut state = state_for(window_bits);
        if state.wrap.allows_gzip_header() {
            assert_eq!(
                inflate_get_header(
                    &mut state,
                    GzHeaderSink::new(
                        Some(extra_out.as_slice()),
                        Some(name_out.as_slice()),
                        Some(comment_out.as_slice()),
                    ),
                ),
                ReturnCode::OK
            );
        }

        let (outcome, adler, next_in) = drive_chunked(&mut state, input, step.max(1));
        let sink = state.head;
        Snapshot {
            outcome,
            adler,
            next_in,
            mode: state.mode,
            check: state.check,
            flags: state.flags,
            wbits: state.wbits,
            done: sink.map_or(GZ_HEADER_PENDING, |head| head.done),
            text: sink.is_some_and(|head| head.text),
            time: sink.map_or(0, |head| head.time),
            xflags: sink.map_or(0, |head| head.xflags),
            os: sink.map_or(0, |head| head.os),
            extra_len: sink.map_or(0, |head| head.extra_len),
            hcrc: sink.is_some_and(|head| head.hcrc),
            extra: contents(&extra_out),
            name: contents(&name_out),
            comment: contents(&comment_out),
        }
    }

    /// Feeding a header one byte at a time is indistinguishable from feeding it all
    /// at once, for every container shape.
    ///
    /// This is the whole resumability contract in one assertion, and it covers both
    /// `NEEDBITS` sites, both zero-terminated fields and the mid-field resume in
    /// `EXTRA`.
    #[test]
    fn chunked_input_matches_a_single_shot() {
        let field = [0xa1_u8, 0xb2, 0xc3, 0xd4, 0xe5];
        let full = Gzip {
            text: true,
            hcrc: true,
            extra: Some(&field),
            name: Some(b"chunked.bin"),
            comment: Some(b"fed one byte at a time"),
            mtime: 0x89ab_cdef,
            xfl: 4,
            operating_system: 7,
            ..Gzip::new()
        }
        .bytes();
        let minimal = Gzip::new().bytes();
        let zlib = zlib_header(15, false).to_vec();
        let dictionary = hex("8 b8 12 34 56 78");

        let cases: [(i32, Vec<u8>); 5] = [
            (AUTO, full),
            (GZIP, minimal),
            (ZLIB, zlib),
            (8, dictionary),
            (AUTO, hex("1f 8b 8 1e 0 0 0 0 0 0 1 0 0 0 0 0 0")),
        ];

        for (window_bits, input) in cases {
            let single = parse(window_bits, &input, input.len().max(1));
            for step in [1_usize, 2, 3, 5, 7, 11] {
                assert_eq!(
                    parse(window_bits, &input, step),
                    single,
                    "windowBits {window_bits} step {step}"
                );
            }
        }
    }

    /// ★ The explicit `NEEDBITS(32)` test: `MTIME`'s four bytes arrive in four
    /// separate calls and must still reassemble (`inflate.c` L576-L578).
    #[test]
    fn mtime_reassembles_across_chunk_boundaries() {
        let input = Gzip {
            mtime: 0x1234_5678,
            ..Gzip::new()
        }
        .bytes();

        // Reveal the four bytes of MTIME one at a time, starting from a chunk that
        // ends in the middle of the field.
        let extra_out = buffer(1);
        let mut state = state_for(GZIP);
        assert_eq!(
            inflate_get_header(
                &mut state,
                GzHeaderSink::new(Some(extra_out.as_slice()), None, None),
            ),
            ReturnCode::OK
        );

        let mut next_in = 0;
        for visible in [5_usize, 6, 7, 8, 9] {
            let (outcome, _) = drive(&mut state, &input[..visible], &mut next_in);
            assert_eq!(
                outcome,
                Outcome::NeedInput,
                "the header is incomplete at {visible} bytes"
            );
        }
        let (outcome, adler) = drive(&mut state, &input, &mut next_in);

        assert_eq!(outcome, Outcome::Header);
        assert_eq!(adler, Some(0));
        assert_eq!(
            state.head.unwrap().time,
            0x1234_5678,
            "no bit of MTIME may be lost across a resume"
        );
    }

    /// An incomplete header consumes only what it could use and stays in the state
    /// it stopped in.
    #[test]
    fn an_incomplete_header_is_resumable_in_place() {
        let input = Gzip {
            name: Some(b"resume"),
            ..Gzip::new()
        }
        .bytes();

        let mut state = state_for(GZIP);
        let mut next_in = 0;
        let (outcome, _) = drive(&mut state, &input[..11], &mut next_in);
        assert_eq!(outcome, Outcome::NeedInput);
        assert_eq!(state.mode, Mode::Name, "stopped inside the file name");
        assert_eq!(next_in, 11, "the name bytes seen so far are consumed");

        let (outcome, _) = drive(&mut state, &input, &mut next_in);
        assert_eq!(outcome, Outcome::Header);
        assert_eq!(next_in, input.len());
    }

    /// An empty chunk never makes progress and never consumes anything, in every
    /// state that can be waiting for input.
    #[test]
    fn an_empty_chunk_is_a_no_op() {
        let input = Gzip {
            extra: Some(&[1, 2, 3]),
            name: Some(b"x"),
            comment: Some(b"y"),
            ..Gzip::new()
        }
        .bytes();

        for prefix in 0..input.len() {
            let mut state = state_for(GZIP);
            let mut next_in = 0;
            let (first, _) = drive(&mut state, &input[..prefix], &mut next_in);
            assert_eq!(first, Outcome::NeedInput, "prefix {prefix}");
            let stopped_at = (state.mode, next_in, state.hold, state.bits, state.length);

            let (second, adler) = drive(&mut state, &input[..prefix], &mut next_in);
            assert_eq!(second, Outcome::NeedInput, "prefix {prefix}");
            assert_eq!(adler, None, "prefix {prefix}");
            assert_eq!(
                (state.mode, next_in, state.hold, state.bits, state.length),
                stopped_at,
                "prefix {prefix}: re-entering with no new input changes nothing"
            );
        }
    }

    // -------------------------------------------------------------------------
    //  The `done` tri-state
    // -------------------------------------------------------------------------

    /// `done` walks 0 -> -1 for a stream that is not gzip, and 0 -> 1 for one that
    /// is (`inflate.c` L1229, L523, L688).
    #[test]
    fn done_reports_all_three_states() {
        // Installed, nothing read yet.
        let out = buffer(4);
        let mut state = state_for(AUTO);
        assert_eq!(
            inflate_get_header(
                &mut state,
                GzHeaderSink::new(Some(out.as_slice()), None, None)
            ),
            ReturnCode::OK
        );
        assert_eq!(state.head.unwrap().done, GZ_HEADER_PENDING);

        // A zlib stream is not a gzip stream.
        let header = zlib_header(15, false);
        let mut next_in = 0;
        assert_eq!(drive(&mut state, &header, &mut next_in).0, Outcome::Header);
        assert_eq!(
            state.head.unwrap().done,
            GZ_HEADER_ABSENT,
            "-1 means: this stream carries no gzip header"
        );

        // A gzip stream reports completion.
        let out = buffer(4);
        let mut state = state_for(AUTO);
        assert_eq!(
            inflate_get_header(
                &mut state,
                GzHeaderSink::new(Some(out.as_slice()), None, None)
            ),
            ReturnCode::OK
        );
        let input = Gzip {
            hcrc: true,
            ..Gzip::new()
        }
        .bytes();
        let mut next_in = 0;
        assert_eq!(drive(&mut state, &input, &mut next_in).0, Outcome::Header);
        assert_eq!(state.head.unwrap().done, GZ_HEADER_COMPLETE);
    }

    /// `done` is set to `-1` even when the stream then turns out not to be valid
    /// zlib either, because the assignment precedes the check (`inflate.c` L522).
    #[test]
    fn done_is_cleared_before_the_zlib_header_is_validated() {
        let out = buffer(1);
        let mut state = state_for(AUTO);
        assert_eq!(
            inflate_get_header(
                &mut state,
                GzHeaderSink::new(Some(out.as_slice()), None, None)
            ),
            ReturnCode::OK
        );
        let input = hex("78 90");
        let mut next_in = 0;

        assert_eq!(
            drive(&mut state, &input, &mut next_in).0,
            Outcome::Bad(MSG_INCORRECT_HEADER_CHECK)
        );
        assert_eq!(state.head.unwrap().done, GZ_HEADER_ABSENT);
    }

    // -------------------------------------------------------------------------
    //  The header CRC
    // -------------------------------------------------------------------------

    /// The header CRC covers exactly the header bytes as they appear on the wire,
    /// up to but not including the CRC-16 itself (RFC 1952 §2.3.1).
    ///
    /// Asserted against a direct [`crc32`] over the same bytes, which pins the
    /// little-endian spill in [`crc2`] and [`crc4`] and the accumulation order
    /// across all six folding sites at once.
    #[test]
    fn header_crc_accumulates_over_the_wire_bytes() {
        let field = [1_u8, 2, 3];
        let input = Gzip {
            hcrc: true,
            extra: Some(&field),
            name: Some(b"nm"),
            comment: Some(b"cm"),
            mtime: 0xfeed_face,
            xfl: 9,
            operating_system: 11,
            ..Gzip::new()
        }
        .bytes();
        let covered = input.len() - 2;

        let mut state = state_for(GZIP);
        let mut next_in = 0;
        let (outcome, _) = drive(&mut state, &input[..covered], &mut next_in);

        assert_eq!(outcome, Outcome::NeedInput);
        assert_eq!(state.mode, Mode::HCrc, "stopped waiting for the CRC-16");
        assert_eq!(next_in, covered);
        assert_eq!(
            state.check,
            crc32(0, &input[..covered]),
            "the header CRC is a plain CRC-32 over the header bytes"
        );
    }

    /// [`crc2`] and [`crc4`] are the `CRC2` and `CRC4` macros
    /// (`inflate.c` L310-L324): a little-endian spill of the accumulator's low two
    /// or four bytes.
    #[test]
    fn crc_helpers_spill_the_accumulator_little_endian() {
        assert_eq!(crc2(0, 0x8b1f), crc32(0, &[0x1f, 0x8b]));
        assert_eq!(crc2(0x1234, 0xbeef), crc32(0x1234, &[0xef, 0xbe]));
        assert_eq!(
            crc4(0x1234, 0xdead_beef),
            crc32(0x1234, &[0xef, 0xbe, 0xad, 0xde])
        );

        // Bits above the field width are ignored, exactly as the C casts to
        // `unsigned char` ignore them.
        assert_eq!(crc2(0, 0xffff_ffff_ffff_beef), crc2(0, 0xbeef));
        assert_eq!(crc4(0, 0xffff_ffff_dead_beef), crc4(0, 0xdead_beef));

        // Successive folds accumulate, which is what lets the header CRC be built
        // one field at a time.
        assert_eq!(crc2(crc2(0, 0x8b1f), 0x0208), crc32(0, &[0x1f, 0x8b, 8, 2]));
    }

    // -------------------------------------------------------------------------
    //  Preset dictionaries
    // -------------------------------------------------------------------------

    /// `test/infcover.c` L410: `8 b8 0 0 0 1` asks for a dictionary.
    ///
    /// Every part of the hand-off is checked: the id is byte-swapped, it is
    /// published in `strm->adler`, the state waits in [`Mode::Dict`], and no further
    /// input is consumed however many times the driver re-enters.
    #[test]
    fn dictionary_id_is_byte_swapped_and_stalls_the_stream() {
        let mut state = state_for(8);
        let input = hex("8 b8 0 0 0 1");
        let mut next_in = 0;
        let (outcome, adler) = drive(&mut state, &input, &mut next_in);

        assert_eq!(outcome, Outcome::NeedDict);
        assert_eq!(state.mode, Mode::Dict);
        assert_eq!(next_in, 6, "the whole header and the id are consumed");
        assert_eq!(
            adler,
            Some(1),
            "ZSWAP32 turns the big-endian id 00 00 00 01 into 1"
        );
        assert_eq!(state.check, 1);

        // Re-entering without a dictionary changes nothing at all.
        let (outcome, adler) = drive(&mut state, &input, &mut next_in);
        assert_eq!(outcome, Outcome::NeedDict);
        assert_eq!(adler, None, "the id is published once, by DICTID");
        assert_eq!(next_in, 6, "Z_NEED_DICT consumes no further input");

        // Once the dictionary is installed the check value restarts from 1.
        state.havedict = true;
        let (outcome, adler) = drive(&mut state, &input, &mut next_in);
        assert_eq!(outcome, Outcome::Header);
        assert_eq!(state.mode, Mode::Type);
        assert_eq!(
            adler,
            Some(1),
            "adler32(0, Z_NULL, 0) again -- inflate.c L705"
        );
        assert_eq!(state.check, 1);
        assert_eq!(next_in, 6);
    }

    /// A four-byte id is reversed byte for byte, not merely re-read
    /// (`zutil.h` L258-L259).
    #[test]
    fn dictionary_id_byte_order_is_reversed() {
        let mut state = state_for(ZLIB);
        let mut input = zlib_header(15, true).to_vec();
        input.extend_from_slice(&[0x12, 0x34, 0x56, 0x78]);
        let mut next_in = 0;
        let (outcome, adler) = drive(&mut state, &input, &mut next_in);

        assert_eq!(outcome, Outcome::NeedDict);
        assert_eq!(adler, Some(0x1234_5678));
        assert_eq!(next_in, 6);
    }

    /// The dictionary id needs all four bytes before it is published
    /// (`inflate.c` L695).
    #[test]
    fn dictionary_id_waits_for_all_four_bytes() {
        let mut state = state_for(ZLIB);
        let mut input = zlib_header(15, true).to_vec();
        input.extend_from_slice(&[0x12, 0x34, 0x56, 0x78]);

        let mut next_in = 0;
        let (outcome, adler) = drive(&mut state, &input[..5], &mut next_in);
        assert_eq!(outcome, Outcome::NeedInput);
        assert_eq!(state.mode, Mode::DictId);
        assert_eq!(
            adler,
            Some(1),
            "the zlib header published its own initial value"
        );

        let (outcome, adler) = drive(&mut state, &input, &mut next_in);
        assert_eq!(outcome, Outcome::NeedDict);
        assert_eq!(adler, Some(0x1234_5678));
    }

    // -------------------------------------------------------------------------
    //  `inflateGetHeader`
    // -------------------------------------------------------------------------

    /// ★ `inflateGetHeader` refuses a stream that was not configured for gzip
    /// (`inflate.c` L1225): it is a `Z_STREAM_ERROR`, not a silent no-op.
    #[test]
    fn inflate_get_header_requires_gzip_decoding() {
        for window_bits in [RAW, ZLIB, 8, 0] {
            let out = buffer(4);
            let mut state = state_for(window_bits);
            assert_eq!(
                inflate_get_header(
                    &mut state,
                    GzHeaderSink::new(Some(out.as_slice()), None, None),
                ),
                ReturnCode::STREAM_ERROR,
                "windowBits {window_bits}"
            );
            assert!(
                state.head.is_none(),
                "a refused install leaves no header behind"
            );
        }

        for window_bits in [GZIP, AUTO, AUTO_FROM_HEADER] {
            let out = buffer(4);
            let mut state = state_for(window_bits);
            assert_eq!(
                inflate_get_header(
                    &mut state,
                    GzHeaderSink::new(Some(out.as_slice()), None, None),
                ),
                ReturnCode::OK,
                "windowBits {window_bits}"
            );
            assert!(state.head.is_some(), "windowBits {window_bits}");
        }
    }

    /// Installing a header always resets `done` to zero, whatever it held before
    /// (`inflate.c` L1229).
    #[test]
    fn inflate_get_header_resets_done() {
        let out = buffer(4);
        let mut sink = GzHeaderSink::new(Some(out.as_slice()), None, None);
        sink.mark_complete();
        assert_eq!(sink.done, GZ_HEADER_COMPLETE);

        let mut state = state_for(GZIP);
        assert_eq!(inflate_get_header(&mut state, sink), ReturnCode::OK);
        assert_eq!(state.head.unwrap().done, GZ_HEADER_PENDING);
    }

    /// A header installed part-way through a stream cannot make the extra-field
    /// offset run backwards: `extra_len - length` wraps, fails the `< extra_max`
    /// guard, and nothing is written (`inflate.c` L616-L617).
    #[test]
    fn a_late_header_install_writes_nothing_into_the_extra_field() {
        let field = [1_u8, 2, 3, 4, 5, 6];
        let input = Gzip {
            hcrc: true,
            extra: Some(&field),
            ..Gzip::new()
        }
        .bytes();

        // Declared before the state so that the state, which borrows it, is
        // dropped first.
        let extra_out = buffer(8);

        // Run without a header until the extra field is half consumed.
        let mut state = state_for(GZIP);
        let mut next_in = 0;
        let (outcome, _) = drive(&mut state, &input[..15], &mut next_in);
        assert_eq!(outcome, Outcome::NeedInput);
        assert_eq!(state.mode, Mode::Extra);
        assert_eq!(
            state.length, 3,
            "three of the six extra bytes are outstanding"
        );

        // Now install a header whose `extra_len` is still zero.
        assert_eq!(
            inflate_get_header(
                &mut state,
                GzHeaderSink::new(Some(extra_out.as_slice()), None, None),
            ),
            ReturnCode::OK
        );
        let (outcome, _) = drive(&mut state, &input, &mut next_in);

        assert_eq!(outcome, Outcome::Header, "the stream still parses");
        assert_eq!(
            contents(&extra_out),
            vec![0xa5; 8],
            "a wrapped offset must copy nothing"
        );
    }

    // -------------------------------------------------------------------------
    //  Reference vectors and robustness
    // -------------------------------------------------------------------------

    /// The header cases of `test/infcover.c`'s `cover_wrap()` (its L399-L411),
    /// with the outcome each one asserts.
    ///
    /// Those calls run the whole of `inflate()`, so the ones that go on to decode
    /// data are represented here by the header outcome alone: reaching
    /// [`Outcome::Header`] is what "the header was accepted" means.
    #[test]
    fn infcover_cover_wrap_header_vectors() {
        let cases: [(&str, i32, Outcome); 9] = [
            (
                "1f 8b 0 0",
                GZIP,
                Outcome::Bad(MSG_UNKNOWN_COMPRESSION_METHOD),
            ),
            (
                "1f 8b 8 80",
                GZIP,
                Outcome::Bad(MSG_UNKNOWN_HEADER_FLAGS_SET),
            ),
            ("77 85", ZLIB, Outcome::Bad(MSG_UNKNOWN_COMPRESSION_METHOD)),
            ("8 99", 0, Outcome::Header),
            ("78 9c", 8, Outcome::Bad(MSG_INVALID_WINDOW_SIZE)),
            ("78 9c 63 0 0 0 1 0 1", ZLIB, Outcome::Header),
            (
                "1f 8b 8 1e 0 0 0 0 0 0 1 0 0 0 0 0 0",
                AUTO,
                Outcome::Bad(MSG_HEADER_CRC_MISMATCH),
            ),
            (
                "1f 8b 8 2 0 0 0 0 0 0 1d 26 3 0 0 0 0 0 0 0 0 0",
                AUTO,
                Outcome::Header,
            ),
            ("78 90", AUTO, Outcome::Bad(MSG_INCORRECT_HEADER_CHECK)),
        ];

        for (vector, window_bits, expected) in cases {
            let input = hex(vector);
            let out = buffer(1);
            let mut state = state_for(window_bits);
            if state.wrap.allows_gzip_header() {
                assert_eq!(
                    inflate_get_header(
                        &mut state,
                        GzHeaderSink::new(
                            Some(out.as_slice()),
                            Some(out.as_slice()),
                            Some(out.as_slice()),
                        ),
                    ),
                    ReturnCode::OK
                );
            }
            let mut next_in = 0;
            let (outcome, _) = drive(&mut state, &input, &mut next_in);
            assert_eq!(outcome, expected, "vector \"{vector}\"");
            assert!(next_in <= input.len(), "vector \"{vector}\"");
        }

        // `test/infcover.c` L410, whose outcome is the dictionary request.
        let input = hex("8 b8 0 0 0 1");
        let mut state = state_for(8);
        let mut next_in = 0;
        assert_eq!(drive(&mut state, &input, &mut next_in).0, Outcome::NeedDict);
    }

    /// A deterministic pseudo-random generator, so that the smoke test below is
    /// reproducible and needs no dependency.
    ///
    /// xorshift64\* -- three shifts and a multiply. The constants are the published
    /// ones; nothing here depends on its statistical quality beyond "covers the
    /// input space quickly".
    struct Rng(u64);

    impl Rng {
        fn next_u32(&mut self) -> u32 {
            let mut x = self.0;
            x ^= x >> 12;
            x ^= x << 25;
            x ^= x >> 27;
            self.0 = x;
            u32::try_from(x.wrapping_mul(0x2545_f491_4f6c_dd1d) >> 32).unwrap()
        }

        fn below(&mut self, bound: u32) -> u32 {
            self.next_u32() % bound
        }
    }

    /// ★ No sequence of header bytes, in any container mode, with or without a
    /// caller header, may panic, hang, over-write a caller buffer or produce an
    /// outcome outside the four the driver understands.
    ///
    /// This is the standing cheap version of `cargo fuzz run fuzz_inflate`, which is
    /// the real gate. Two properties beyond "did not panic" are asserted: a
    /// rejection always carries one of the five documented messages, and the
    /// sentinel bytes past every caller buffer's advertised capacity are never
    /// disturbed -- the buffers are deliberately tiny and aliased, which is the
    /// shape `test/infcover.c` uses.
    #[test]
    fn random_headers_never_panic_and_never_overrun() {
        let messages = [
            MSG_INCORRECT_HEADER_CHECK,
            MSG_UNKNOWN_COMPRESSION_METHOD,
            MSG_INVALID_WINDOW_SIZE,
            MSG_UNKNOWN_HEADER_FLAGS_SET,
            MSG_HEADER_CRC_MISMATCH,
        ];
        let modes = [RAW, ZLIB, GZIP, AUTO, AUTO_FROM_HEADER, 0, 8];
        let mut rng = Rng(0x2b6f_1e51_9c4d_a7f3);

        for round in 0..4096_u32 {
            let len = usize::try_from(rng.below(28)).unwrap();
            let mut input = Vec::with_capacity(len);
            for _ in 0..len {
                // A quarter of the streams start with the gzip magic, so that the
                // gzip states are reached far more often than by chance.
                input.push(u8::try_from(rng.below(256)).unwrap());
            }
            if round % 4 == 0 && input.len() >= 2 {
                input[0] = 0x1f;
                input[1] = 0x8b;
            }
            if round % 8 == 3 && input.len() >= 2 {
                let header = zlib_header(15, round % 16 == 3);
                input[0] = header[0];
                input[1] = header[1];
            }

            let window_bits = modes[usize::try_from(rng.below(7)).unwrap()];
            // Capacity 2, with three bytes of sentinel behind it, so that any write
            // past the advertised space is visible.
            let backing = buffer(5);
            let advertised = &backing[..2];
            let mut state = state_for(window_bits);
            if state.wrap.allows_gzip_header() && round % 3 != 0 {
                assert_eq!(
                    inflate_get_header(
                        &mut state,
                        GzHeaderSink::new(Some(advertised), Some(advertised), Some(advertised)),
                    ),
                    ReturnCode::OK
                );
            }

            let step = usize::try_from(rng.below(4)).unwrap() + 1;
            let (outcome, _, next_in) = drive_chunked(&mut state, &input, step);

            assert!(next_in <= input.len(), "round {round}");
            if let Outcome::Bad(msg) = outcome {
                assert!(messages.contains(&msg), "round {round}: unexpected {msg:?}");
                assert_eq!(state.mode, Mode::Bad, "round {round}");
            }
            assert_eq!(
                contents(&backing)[2..],
                [0xa5, 0xa5, 0xa5],
                "round {round}: wrote past the advertised capacity"
            );
        }
    }

    /// The exit protocol itself: `Advance` publishes nothing, a rejection publishes
    /// a message and no check value, and `NeedDict` publishes neither.
    #[test]
    fn exit_values_carry_only_what_the_reference_writes() {
        assert_eq!(HeaderExit::advance().adler, None);
        assert_eq!(HeaderExit::advance().msg, None);
        assert_eq!(HeaderExit::advance_with_adler(7).adler, Some(7));
        assert_eq!(HeaderExit::advance_with_adler(7).msg, None);
        assert_eq!(HeaderExit::need_input().action, HeaderAction::NeedInput);
        assert_eq!(HeaderExit::need_input().adler, None);
        assert_eq!(HeaderExit::need_dict().action, HeaderAction::NeedDict);
        assert_eq!(HeaderExit::need_dict().adler, None);
        assert_eq!(HeaderExit::need_dict().msg, None);

        let mut state = state_for(ZLIB);
        let exit = super::reject(&mut state, MSG_INVALID_WINDOW_SIZE);
        assert_eq!(exit.action, HeaderAction::Bad);
        assert_eq!(exit.msg, Some(MSG_INVALID_WINDOW_SIZE));
        assert_eq!(exit.adler, None);
        assert_eq!(state.mode, Mode::Bad);
    }

    /// The five messages are the reference's strings, character for character
    /// (`inflate.c` L529, L534, L543, L564, L680).
    #[test]
    fn messages_match_the_reference_strings() {
        assert_eq!(MSG_INCORRECT_HEADER_CHECK, "incorrect header check");
        assert_eq!(MSG_UNKNOWN_COMPRESSION_METHOD, "unknown compression method");
        assert_eq!(MSG_INVALID_WINDOW_SIZE, "invalid window size");
        assert_eq!(MSG_UNKNOWN_HEADER_FLAGS_SET, "unknown header flags set");
        assert_eq!(MSG_HEADER_CRC_MISMATCH, "header crc mismatch");
    }
}
