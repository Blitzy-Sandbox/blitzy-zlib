//! Typed compression and decompression parameters, and the accept/reject
//! boundaries the reference implementation applies to them.
//!
//! `deflateInit2_` and `inflateInit2_` take their parameters as bare C `int`s
//! (`zlib.h` L1907-L1912), and every one of those integers carries a meaning
//! that the type `int` does not express: a level of `-1` is not "minus one" but
//! "pick the default", a `windowBits` of `25` is not a 32 MiB window but "a
//! 512-byte window wrapped in a gzip container", and a `windowBits` of `0` means
//! one thing to inflate and nothing at all to deflate. This module is where
//! those integers become types, so that the rest of the engine works with
//! [`Strategy`], [`Wrap`] and [`Flush`] instead of re-deriving what each `int`
//! meant.
//!
//! # Correspondence with the reference implementation
//!
//! The module is a mirror of the *parameter handling* of five C functions, and of
//! the constant blocks those functions test against:
//!
//! | Rust | C original |
//! |---|---|
//! | [`validate_deflate_params`] | `deflateInit2_`, `deflate.c` L419-L439 |
//! | [`decode_deflate_window_bits`] | the `windowBits` decode inside it, `deflate.c` L422-L439 |
//! | [`InflateConfig::validate`] / [`decode_inflate_window_bits`] | `inflateReset2`, `inflate.c` L145-L161 |
//! | [`validate_inflate_back_window_bits`] | `inflateBackInit_`, `infback.c` L33-L35 |
//! | [`validate_deflate_params_change`] | `deflateParams`, `deflate.c` L784-L788 |
//! | [`validate_deflate_flush`] | the flush guard of `deflate`, `deflate.c` L985 |
//! | [`DeflateConfig::default`] | `deflateInit_`, `deflate.c` L379-L384 |
//! | [`InflateConfig::default`] | `inflateInit_`, `inflate.c` L214-L217 |
//! | the `Z_*` constants | `zlib.h` L172-L213 |
//! | the window and memory bounds | `zconf.h` L272-L288, `zutil.h` L72-L96 |
//!
//! # Why the two directions are kept apart
//!
//! `windowBits` is the parameter most likely to be got wrong, because
//! deflate and inflate do **not** agree on what it means. Reproducing one set of
//! rules for both is the single most likely defect in this module, so
//! the two decoders are separate functions with separate result types and
//! separate tests:
//!
//! * deflate promotes a request for `8` to `9`, because the current
//!   implementation cannot use a 256-byte window (`deflate.c` L439, explained at
//!   `zlib.h` L562-L568). Inflate performs no such promotion.
//! * deflate rejects `8` outright for raw and gzip streams, since only the zlib
//!   header can transmit a window size to the decompressor (`deflate.c` L436,
//!   `zlib.h` L582-L584). Inflate accepts `-8`.
//! * inflate accepts `0`, meaning "take the window size from the stream header"
//!   (`inflate.c` L160, `zlib.h` L875-L876). For deflate `0` is an error.
//! * inflate accepts `+32`, meaning "detect a zlib or a gzip header
//!   automatically" (`inflate.c` L152, `zlib.h` L890-L892). For deflate no such
//!   mode exists.
//!
//! # What must not change, and why
//!
//! Every bound here is *observable*: a configuration the C library accepts must
//! be accepted, and one it rejects must be rejected with the same status code.
//! `test/example.c` and `test/infcover.c` both drive edge parameters
//! deliberately, and the differential test matrix enumerates level 0-9 x
//! `windowBits` (raw, zlib and gzip forms) x `memLevel` 1-9 x five strategies x
//! six flush modes through exactly these functions. Tightening a bound by one
//! would reject a stream the reference compresses; loosening one would let the
//! engine allocate a state the reference refuses to build. Neither is a
//! "hardening" improvement -- both are behaviour changes.
//!
//! Two consequences follow, and they are the reason this module exists at all
//! rather than the checks living where they are used:
//!
//! 1. **The bounds are written down once.** `deflate/mod.rs`, `inflate/mod.rs`,
//!    `infback.rs`, `compress.rs` and `uncompress.rs` all reach their limits
//!    through the functions here, so they agree by construction rather than by
//!    review.
//! 2. **Compile-time knobs are not parameters.** The port implements the default
//!    configuration of the reference build and nothing else, so none of the
//!    knobs below appears here as a field, a feature or a runtime switch. They
//!    do not all do the same thing, though, and it is worth separating them
//!    rather than asserting that each one moves a byte:
//!
//!    * **Output-changing.** `FASTEST` (a two-entry `configuration_table` plus a
//!      cut-down `longest_match`, `deflate.c` L106-L110 and L1532-L1588),
//!      `UNALIGNED_OK` (the alternative `longest_match` body, `deflate.c`
//!      L1404-L1512) and `FORCE_STATIC` / `FORCE_STORED` (block-type overrides,
//!      `trees.c` L1034 and L1044) each change what the encoder emits. These are
//!      the ones byte-identity is directly sensitive to.
//!    * **Output-neutral, and excluded for other reasons.** `LIT_MEM` moves the
//!      symbol buffer and raises `LIT_BUFS` from 4 to 5 (`deflate.h` L224-L231),
//!      but the block-flush threshold is the same symbol count either way
//!      (`deflate.c` L515-L521), so emitted bytes do not move -- what moves is
//!      per-stream memory. `GEN_TREES_H` computes the static trees at run time
//!      instead of using the committed `trees.h` (`trees.c` L83, L296,
//!      L370-L435); the values are identical. `DYNAMIC_CRC_TABLE` likewise
//!      computes the CRC tables at run time rather than using the committed
//!      `crc32.h` constants, which `crc32.c` itself generated, so **checksum
//!      values and compressed output are unaffected**; it is excluded because it
//!      flips `zlibCompileFlags()` bit 13 and because `crc32.c` L13-L17 records
//!      that no mutex guards the construction.
//!
//!    `GZIP` (`deflate.h` L22-L23) and `GUNZIP` (`inflate.h` L15-L16) are the
//!    exception that proves the rule: both are defined by default, so the gzip
//!    wrapper paths are ported unconditionally.
//!
//! # Layering and safety posture
//!
//! `no_std`, allocation-free, and dependency-free apart from
//! [`ReturnCode`]: it names only `core` and
//! `crate::error`. Nothing here is `#[repr(C)]`, `#[no_mangle]` or `extern "C"`,
//! and nothing here is a raw pointer -- turning these types back into the C
//! `int`s a caller passed is the business of the `libz-rs-sys` facade, which is
//! the only crate in the workspace allowed to **use** `unsafe`, and therefore the
//! only one that can dereference a pointer or make an FFI call. (That is the
//! accurate form of the claim. This crate does hold two raw pointer *values* --
//! `allocate::Opaque` and `gz::state::GzFileExposed::next` -- neither of which it
//! can read through, and neither of which is in this module.) Every conversion is
//! total and fallible: no input,
//! including [`i32::MIN`] and [`i32::MAX`], can make any function here panic,
//! and every arithmetic step of the `windowBits` decode is checked.
//!
//! # Examples
//!
//! ```
//! use zlib_rs::config::{decode_deflate_window_bits, decode_inflate_window_bits};
//! use zlib_rs::{DeflateConfig, InflateWrap, Wrap, DEF_MEM_LEVEL, MAX_WBITS, Z_DEFAULT_COMPRESSION};
//!
//! // The defaults `deflateInit` and `inflateInit` supply.
//! let deflate = DeflateConfig::default();
//! assert_eq!(deflate.level, Z_DEFAULT_COMPRESSION);
//! assert_eq!(deflate.window_bits, MAX_WBITS);
//! assert_eq!(deflate.mem_level, DEF_MEM_LEVEL);
//!
//! // A default-level zlib stream: level -1 resolves to 6, window bits stay 15.
//! let validated = deflate.validate().map(|v| (v.level, v.wrap, v.window_bits));
//! assert_eq!(validated, Ok((6, Wrap::Zlib, 15)));
//!
//! // Deflate promotes a 256-byte window request to 512 bytes ...
//! assert_eq!(decode_deflate_window_bits(8), Ok((Wrap::Zlib, 9)));
//! // ... but inflate does not.
//! assert_eq!(decode_inflate_window_bits(-8), Ok((InflateWrap::None, 8)));
//! // Only inflate understands "take the window size from the header".
//! assert_eq!(decode_inflate_window_bits(0), Ok((InflateWrap::Zlib, 0)));
//! assert!(decode_deflate_window_bits(0).is_err());
//! ```
//!
//! [`decode_deflate_window_bits`]: crate::config::decode_deflate_window_bits
//! [`decode_inflate_window_bits`]: crate::config::decode_inflate_window_bits
//! [`validate_deflate_flush`]: crate::config::validate_deflate_flush
//! [`validate_deflate_params`]: crate::config::validate_deflate_params
//! [`validate_deflate_params_change`]: crate::config::validate_deflate_params_change
//! [`validate_inflate_back_window_bits`]: crate::config::validate_inflate_back_window_bits

// The definitions closing the module documentation above are link-reference definitions, not
// prose: a module whose `mod` declaration carries an outer doc comment has its `//!` block
// resolved in the scope of the DECLARING module, so an unqualified sibling name does not
// resolve. See "Documentation lints" in `src/lib.rs` for the rule and the gate.

// `DeflateConfig`, `InflateConfig`, `ValidatedDeflateConfig` and
// `ValidatedInflateConfig` all end in the name of the module that holds them,
// which is what `clippy::module_name_repetitions` objects to. The names are kept
// anyway: they are the names the parameter sets of `deflateInit2_` and
// `inflateInit2_` are known by throughout the zlib ecosystem, and shortening
// them to `Deflate` and `Inflate` would collide with the engine types those
// modules define. The alternative -- moving the types out of `config` -- would
// scatter the bounds this module exists to centralise.
#![allow(clippy::module_name_repetitions)]

use crate::error::ReturnCode;

/// `Z_NO_COMPRESSION` (0) -- store the input without compressing it.
///
/// Mirrors `zlib.h` L194.
pub const Z_NO_COMPRESSION: i32 = 0;

/// `Z_BEST_SPEED` (1) -- the fastest of the ten levels, and the weakest.
///
/// Mirrors `zlib.h` L195.
pub const Z_BEST_SPEED: i32 = 1;

/// `Z_BEST_COMPRESSION` (9) -- the slowest of the ten levels, and the strongest.
///
/// Mirrors `zlib.h` L196.
pub const Z_BEST_COMPRESSION: i32 = 9;

/// `Z_DEFAULT_COMPRESSION` (-1) -- "use the library's default level", which is
/// [`DEF_LEVEL`].
///
/// This is the one negative value the `level` parameter accepts, and it is
/// resolved to [`DEF_LEVEL`] *before* the range check rather than after, which is
/// why `-1` is accepted while `-2` is not (`deflate.c` L419 then L435).
///
/// Mirrors `zlib.h` L197.
pub const Z_DEFAULT_COMPRESSION: i32 = -1;

/// The level [`Z_DEFAULT_COMPRESSION`] resolves to: 6.
///
/// The reference implementation spells this as a bare literal in two places,
/// `deflate.c` L419 (`deflateInit2_`) and `deflate.c` L784 (`deflateParams`);
/// both reach it through [`normalize_deflate_level`] here.
///
/// This is *not* the same thing as [`DEF_MEM_LEVEL`], and the two are easy to
/// confuse because the reference has no name for this one.
///
/// Mirrors `deflate.c` L419.
pub const DEF_LEVEL: i32 = 6;

/// `Z_DEFAULT_STRATEGY` (0) -- normal data; full string matching.
///
/// Mirrors `zlib.h` L204.
pub const Z_DEFAULT_STRATEGY: i32 = 0;

/// `Z_FILTERED` (1) -- data produced by a filter or predictor, such as the PNG
/// filters: more Huffman coding and less string matching than the default.
///
/// Mirrors `zlib.h` L200.
pub const Z_FILTERED: i32 = 1;

/// `Z_HUFFMAN_ONLY` (2) -- Huffman coding only, with no string matching at all.
///
/// Mirrors `zlib.h` L201.
pub const Z_HUFFMAN_ONLY: i32 = 2;

/// `Z_RLE` (3) -- limit match distances to one, which is run-length encoding.
///
/// Mirrors `zlib.h` L202.
pub const Z_RLE: i32 = 3;

/// `Z_FIXED` (4) -- default string matching, but never dynamic Huffman codes.
///
/// Also the upper bound of the `strategy` range check: the reference tests
/// `strategy < 0 || strategy > Z_FIXED` (`deflate.c` L436).
///
/// Mirrors `zlib.h` L203.
pub const Z_FIXED: i32 = 4;

/// `Z_BINARY` (0) -- the `data_type` value for binary data.
///
/// Mirrors `zlib.h` L207.
pub const Z_BINARY: i32 = 0;

/// `Z_TEXT` (1) -- the `data_type` value for text.
///
/// Mirrors `zlib.h` L208.
pub const Z_TEXT: i32 = 1;

/// `Z_ASCII` (1) -- a deprecated alias of [`Z_TEXT`], kept because callers
/// written against zlib 1.2.2 and earlier still spell it this way.
///
/// The reference defines it as `#define Z_ASCII Z_TEXT`, so the two are the same
/// value by construction here as well, rather than by coincidence.
///
/// Mirrors `zlib.h` L209.
pub const Z_ASCII: i32 = Z_TEXT;

/// `Z_UNKNOWN` (2) -- the `data_type` value for data not yet classified.
///
/// Mirrors `zlib.h` L210.
pub const Z_UNKNOWN: i32 = 2;

/// `Z_DEFLATED` (8) -- the deflate compression method, and the only value the
/// `method` parameter accepts (`deflate.c` L434).
///
/// The same integer is also the `CM` nibble of the zlib header
/// (`doc/rfc1950.txt`) and the `CM` byte of the gzip header
/// (`doc/rfc1952.txt`), which is why `inflate` compares against it when parsing
/// a header (`inflate.c` L533 and L558) as well as when validating a parameter.
///
/// Mirrors `zlib.h` L213.
pub const Z_DEFLATED: i32 = 8;

/// `Z_NO_FLUSH` (0) -- accumulate input and decide for yourself when to emit.
///
/// Mirrors `zlib.h` L172.
pub const Z_NO_FLUSH: i32 = 0;

/// `Z_PARTIAL_FLUSH` (1) -- flush all pending output **without** byte alignment.
///
/// `zlib.h` L300-L306 is explicit that "the output is **not** aligned to a byte
/// boundary": the current deflate block is completed and followed by an empty
/// *fixed-codes* block that is 10 bits long, which is what assures the
/// decompressor receives enough bits to finish the real block. Contrast
/// [`Z_SYNC_FLUSH`], which does align, using an empty *stored* block of three
/// bits plus filler to the next byte followed by `00 00 ff ff` (`zlib.h`
/// L296-L298).
///
/// Mirrors `zlib.h` L173.
pub const Z_PARTIAL_FLUSH: i32 = 1;

/// `Z_SYNC_FLUSH` (2) -- flush and align to a byte boundary with an empty stored
/// block.
///
/// Mirrors `zlib.h` L174.
pub const Z_SYNC_FLUSH: i32 = 2;

/// `Z_FULL_FLUSH` (3) -- as [`Z_SYNC_FLUSH`], and reset the compression state so
/// that decompression can restart from this point.
///
/// Mirrors `zlib.h` L175.
pub const Z_FULL_FLUSH: i32 = 3;

/// `Z_FINISH` (4) -- no more input is coming; finish the stream.
///
/// Mirrors `zlib.h` L176.
pub const Z_FINISH: i32 = 4;

/// `Z_BLOCK` (5) -- stop at a block boundary.
///
/// Also the upper bound of `deflate`'s flush guard, which rejects anything above
/// it (`deflate.c` L985); see [`Flush::is_valid_for_deflate`].
///
/// Mirrors `zlib.h` L177.
pub const Z_BLOCK: i32 = 5;

/// `Z_TREES` (6) -- stop after the block header, for `inflate` only.
///
/// Mirrors `zlib.h` L178.
pub const Z_TREES: i32 = 6;

/// The smallest window exponent either direction documents: 8, a 256-byte
/// window.
///
/// The reference has no name for this bound and spells it as the literal `8` in
/// `deflate.c` L435, `inflate.c` L160 and `infback.c` L34; `zlib.h` L556-L557 and
/// L866-L867 are where it is documented.
///
/// **Accepting `8` is not the same as using it**, and the distinction has its own
/// constant elsewhere in the crate. This one is the smallest *argument* a caller
/// may pass. The smallest exponent a live `deflate_state` can hold is **9**,
/// because `deflate.c` L439 promotes 8 immediately, and that is what
/// `weak_slice::MIN_WBITS` states -- a `u32`, because it belongs to the resolved
/// window-buffer domain rather than to the signed C parameter domain. The two are
/// deliberately different numbers and must not be substituted for one another.
///
/// Mirrors `zlib.h` L556-L557 and `deflate.c` L435.
pub const MIN_WBITS: i32 = 8;

/// `MAX_WBITS` (15) -- the largest window exponent, a 32 KiB LZ77 window.
///
/// `zconf.h` L281-L285 warns that reducing it makes `minigzip` unable to extract
/// `.gz` files created by `gzip`, and `zutil.h` L72-L74 refuses to compile at all
/// unless it lies in `9..=15`. The implementation implements the shipped value and offers
/// no way to change it, because a smaller window would change the bytes the
/// encoder emits.
///
/// The same integer serves as the mask that extracts the window exponent from an
/// inflate `windowBits` request, where the reference writes it as the literal
/// `15` (`inflate.c` L155); that use is spelled out separately as
/// `INFLATE_WINDOW_BITS_MASK` so the two roles are not conflated.
///
/// `weak_slice::MAX_WBITS` carries the same 15 as a `u32` for the resolved
/// window-buffer domain. This `i32` spelling is the parameter-validation domain,
/// where a request may still be negative or carry a `+16` offset, so the two are
/// not interchangeable even though they agree numerically.
///
/// Mirrors `zconf.h` L286-L288.
pub const MAX_WBITS: i32 = 15;

/// `DEF_WBITS` (15) -- the default window exponent for **decompression**.
///
/// `zutil.h` L78 is emphatic that this is a separate constant from
/// [`MAX_WBITS`], which "is for compression only", even though the shipped build
/// defines one as the other. `inflateInit_` passes this value to
/// `inflateInit2_` (`inflate.c` L216).
///
/// Mirrors `zutil.h` L75-L78.
pub const DEF_WBITS: i32 = MAX_WBITS;

/// The smallest `memLevel`: 1, minimum memory, slowest, worst ratio.
///
/// The reference spells this bound as the literal `1` in `deflate.c` L434 and
/// documents it at `zlib.h` L586-L590.
///
/// Mirrors `deflate.c` L434.
pub const MIN_MEM_LEVEL: i32 = 1;

/// `MAX_MEM_LEVEL` (9) -- the largest `memLevel`, maximum memory and optimal
/// speed.
///
/// **Nine, not eight.** `zconf.h` L272-L279 defines it as 9 except under
/// `MAXSEG_64K`, a 16-bit segmented-memory configuration the implementation does not
/// implement; [`DEF_MEM_LEVEL`] is the one that is 8. Conflating the two is an
/// easy mistake with observable consequences: it would reject `memLevel = 9`,
/// which the reference accepts.
///
/// Mirrors `zconf.h` L272-L279.
pub const MAX_MEM_LEVEL: i32 = 9;

/// `DEF_MEM_LEVEL` (8) -- the default `memLevel`, and the value `deflateInit_`
/// passes to `deflateInit2_` (`deflate.c` L381).
///
/// `zutil.h` L80-L84 derives it as `8` whenever [`MAX_MEM_LEVEL`] is at least 8,
/// and as [`MAX_MEM_LEVEL`] otherwise; with the shipped `MAX_MEM_LEVEL` of 9 the
/// first arm applies.
///
/// Mirrors `zutil.h` L80-L84.
pub const DEF_MEM_LEVEL: i32 = 8;

//  Engine constants shared by the deflate and trees subsystems -- `zutil.h`
//  L87-L96

/// `MIN_MATCH` (3) -- the shortest length an LZ77 match may have.
///
/// A `usize` rather than an `i32` because every use is a length or a window
/// index: the lookahead comparisons in `longest_match`, the `INSERT_STRING` hash
/// update, and the `hash_shift` derivation `(hash_bits + MIN_MATCH - 1) /
/// MIN_MATCH` (`deflate.c` L456).
///
/// This and [`MAX_MATCH`] are stated identically -- same name, same `usize`, same
/// value -- by `weak_slice::MIN_MATCH` and `weak_slice::MAX_MATCH`, which need
/// them to bound the window views they hand out. They are two spellings of one
/// number from `zutil.h`, so a crate root that re-exports both modules should
/// re-export the pair from one of them rather than glob both into the same
/// namespace.
///
/// Mirrors `zutil.h` L92.
pub const MIN_MATCH: usize = 3;

/// `MAX_MATCH` (258) -- the longest length an LZ77 match may have.
///
/// Mirrors `zutil.h` L93.
pub const MAX_MATCH: usize = 258;

/// `PRESET_DICT` (0x20) -- the `FDICT` flag of the zlib header.
///
/// Bit 5 of the header's second byte, which the reference sets by OR-ing this
/// value into the 16-bit header word it is about to write (`deflate.c` L1045, on
/// a `uInt`), hence the `u32`. `inflate` reads the same flag out of its bit
/// accumulator after dropping the four `CM` bits, where it has moved to `0x200`
/// (`inflate.c` L551) -- so this constant belongs to the emit path and the
/// decode path's shifted spelling is not interchangeable with it.
///
/// Mirrors `zutil.h` L96.
pub const PRESET_DICT: u32 = 0x20;

/// `STORED_BLOCK` (0) -- the block type of an uncompressed stored block.
///
/// The three block-type tags are two-bit values that reach the bitstream as
/// `(tag << 1) + last` in a three-bit field (`trees.c` L862, L1059 and L1066),
/// so they are `u8` here and widen losslessly wherever the bit writer wants them.
///
/// Mirrors `zutil.h` L87.
pub const STORED_BLOCK: u8 = 0;

/// `STATIC_TREES` (1) -- the block type of a block coded with the fixed Huffman
/// trees.
///
/// Mirrors `zutil.h` L88.
pub const STATIC_TREES: u8 = 1;

/// `DYN_TREES` (2) -- the block type of a block coded with per-block dynamic
/// Huffman trees.
///
/// Mirrors `zutil.h` L89.
pub const DYN_TREES: u8 = 2;

/// Bit 0 of `inflate_state.wrap`: a zlib header is acceptable.
///
/// `inflate.c` L524 tests `!(state->wrap & 1)` to decide whether the two bytes it
/// is looking at may be read as a zlib header.
///
/// Mirrors `inflate.c` L524.
pub const INFLATE_WRAP_ZLIB_HEADER: i32 = 1;

/// Bit 1 of `inflate_state.wrap`: a gzip header is acceptable.
///
/// `inflate.c` L513 tests `(state->wrap & 2) && hold == 0x8b1f` to recognise the
/// gzip magic, and `inflateGetHeader` refuses outright when the bit is clear
/// (`inflate.c` L1225).
///
/// Mirrors `inflate.c` L513.
pub const INFLATE_WRAP_GZIP_HEADER: i32 = 2;

/// Bit 2 of `inflate_state.wrap`: the stream's check value is to be verified.
///
/// Set for every wrapped mode by the `+ 5` bias of `inflate.c` L152, cleared by
/// `inflateValidate(strm, 0)` (`inflate.c` L1390-L1393) and consulted at each
/// trailer comparison (`inflate.c` L679, L1078-L1100). Clearing it later is a
/// state transition, not a configuration choice, so it is
/// `inflate/state.rs`'s business rather than this module's.
///
/// Mirrors `inflate.c` L152 and L1390-L1393.
pub const INFLATE_WRAP_VERIFY_CHECK: i32 = 4;

/// The offset added to `windowBits` to ask deflate for a gzip wrapper, and
/// subtracted again to recover the window exponent.
///
/// Mirrors `zlib.h` L574-L576 and `deflate.c` L431.
const GZIP_WRAP_OFFSET: i32 = 16;

/// The mask that extracts the window exponent from a non-negative inflate
/// `windowBits` request: the reference's literal `15` at `inflate.c` L155.
///
/// Numerically equal to [`MAX_WBITS`] because the exponent occupies the low
/// nibble, but a different quantity: this one is a bit mask.
const INFLATE_WINDOW_BITS_MASK: i32 = 0xF;

/// The shift that turns an inflate `windowBits` request into a wrap request:
/// `wrap = (windowBits >> 4) + 5` (`inflate.c` L152).
const INFLATE_WRAP_SHIFT: u32 = 4;

/// The first inflate `windowBits` request whose low nibble is *not* masked off:
/// 48. Anything at or above it therefore keeps its full value and fails the
/// window-exponent range check (`inflate.c` L154-L155 then L160-L161).
const INFLATE_WRAP_MASK_LIMIT: i32 = 48;

/// `inflate_state.wrap` for a raw stream: no header, no trailer, no check value.
///
/// Mirrors `inflate.c` L148.
const INFLATE_WRAP_RAW: i32 = 0;

/// `inflate_state.wrap` for a zlib stream: 5.
///
/// Mirrors `inflate.c` L152 with `windowBits` in `0..=15`.
const INFLATE_WRAP_ZLIB_ONLY: i32 = INFLATE_WRAP_ZLIB_HEADER | INFLATE_WRAP_VERIFY_CHECK;

/// The bias of `wrap = (windowBits >> 4) + 5`, which is exactly the zlib wrap
/// value: a request in `0..=15` shifts to 0 and so yields 5 (zlib header plus
/// check value), `16..=31` shifts to 1 and yields 6 (gzip header plus check
/// value), and `32..=47` shifts to 2 and yields 7 (either header plus check
/// value). The progression is arithmetic rather than bitwise, which is why the
/// three results are named individually below instead of being composed from the
/// bit constants at the point of use.
///
/// Mirrors `inflate.c` L152.
const INFLATE_WRAP_BIAS: i32 = INFLATE_WRAP_ZLIB_ONLY;

/// `inflate_state.wrap` for a gzip-only stream: 6.
///
/// Mirrors `inflate.c` L152 with `windowBits` in `16..=31`.
const INFLATE_WRAP_GZIP_ONLY: i32 = INFLATE_WRAP_GZIP_HEADER | INFLATE_WRAP_VERIFY_CHECK;

/// `inflate_state.wrap` for automatic zlib-or-gzip detection: 7.
///
/// Mirrors `inflate.c` L152 with `windowBits` in `32..=47`.
const INFLATE_WRAP_AUTODETECT: i32 =
    INFLATE_WRAP_ZLIB_HEADER | INFLATE_WRAP_GZIP_HEADER | INFLATE_WRAP_VERIFY_CHECK;

/// The `windowBits` request that means "take the window size from the stream
/// header", which only inflate honours (`inflate.c` L160, `zlib.h` L875-L876).
const WINDOW_BITS_FROM_HEADER: i32 = 0;

/// [`MAX_WBITS`] as a `u8`, for the resolved window exponents that this module
/// hands out. The two are pinned to each other by a test rather than by a cast,
/// so that neither can drift.
const MAX_WBITS_U8: u8 = 15;

/// The compression method of a deflate stream.
///
/// The `method` parameter of `deflateInit2_` is an `int` with exactly one legal
/// value, [`Z_DEFLATED`]; `deflate.c` L434 rejects everything else with
/// `Z_STREAM_ERROR`. Modelling it as a single-variant enum rather than as an
/// `i32` is what makes "some other method" unrepresentable once a configuration
/// has been built, and leaves the rejection to happen in exactly one place --
/// [`Method::from_raw`].
///
/// The enum is deliberately not `#[non_exhaustive]`. `zlib.h` L214 describes
/// deflate as "the only one supported in this version", and adding a second
/// compression method would be a new wire format, which the frozen capability surface
/// excludes by construction.
///
/// Mirrors `zlib.h` L213-L214 and `deflate.c` L434.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Method {
    /// The DEFLATE method of RFC 1951, `Z_DEFLATED` (8).
    ///
    /// The default, because it is the only value: `deflateInit_` passes
    /// [`Z_DEFLATED`] on the caller's behalf (`deflate.c` L381).
    #[default]
    Deflated = 8,
}

impl Method {
    /// Every method this version of the library implements: just the one.
    ///
    /// Mirrors `zlib.h` L213-L214.
    pub const ALL: [Self; 1] = [Self::Deflated];

    /// Returns the C `int` that names this method.
    ///
    /// Mirrors `zlib.h` L213.
    #[must_use]
    pub const fn as_raw(self) -> i32 {
        match self {
            Self::Deflated => Z_DEFLATED,
        }
    }

    /// Converts a raw `method` argument into a [`Method`], returning [`None`] for
    /// any value the reference rejects.
    ///
    /// Total over `i32`: every input, [`i32::MIN`] and [`i32::MAX`] included,
    /// either names [`Method::Deflated`] or is refused.
    ///
    /// Mirrors the `method != Z_DEFLATED` term of `deflate.c` L434.
    #[must_use]
    pub const fn from_raw(raw: i32) -> Option<Self> {
        match raw {
            Z_DEFLATED => Some(Self::Deflated),
            _ => None,
        }
    }

    /// The reference spelling of this method, for diagnostics.
    ///
    /// Mirrors `zlib.h` L213.
    #[must_use]
    pub const fn c_name(self) -> &'static str {
        match self {
            Self::Deflated => "Z_DEFLATED",
        }
    }
}

impl TryFrom<i32> for Method {
    type Error = ReturnCode;

    /// Converts a raw `method` argument into a [`Method`].
    ///
    /// The fallible-conversion form of [`Method::from_raw`], for call sites that
    /// propagate a status code with `?`. [`Method::from_raw`] is the one to use
    /// in a `const` context, because a trait method cannot be `const`.
    ///
    /// # Errors
    ///
    /// Returns [`ReturnCode::STREAM_ERROR`] for any value other than
    /// [`Z_DEFLATED`], which is the code `deflateInit2_` returns for an invalid
    /// method (`deflate.c` L434-L437).
    fn try_from(raw: i32) -> Result<Self, Self::Error> {
        Self::from_raw(raw).ok_or(ReturnCode::STREAM_ERROR)
    }
}

/// How the encoder should trade string matching against Huffman coding.
///
/// The five values of the `strategy` parameter, in the order `zlib.h` L200-L204
/// declares them by value. `zlib.h` L604-L606 records the property that makes
/// this parameter safe to expose as an unconstrained choice: strategy "affects
/// the compression ratio but never the correctness of the compressed output".
/// The degree of string matching, from most to none, is [`Default`](Self::Default),
/// [`Filtered`](Self::Filtered), [`Rle`](Self::Rle), then
/// [`HuffmanOnly`](Self::HuffmanOnly); [`Fixed`](Self::Fixed) is orthogonal to
/// that scale -- it matches as usual but forbids dynamic Huffman codes.
///
/// The discriminants are written as the C integers so that the declaration can
/// be diffed against `zlib.h` directly, but nothing observes them: no `#[repr]`
/// is declared, no caller can see a `Strategy`, and the conversion to and from
/// the ABI value goes through [`Strategy::as_raw`] and [`Strategy::from_raw`]
/// rather than through a cast.
///
/// Mirrors `zlib.h` L200-L204.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Strategy {
    /// `Z_DEFAULT_STRATEGY` (0) -- normal data, full string matching.
    #[default]
    Default = 0,
    /// `Z_FILTERED` (1) -- filtered or predicted data, such as PNG image rows.
    Filtered = 1,
    /// `Z_HUFFMAN_ONLY` (2) -- Huffman coding only, no string matching.
    HuffmanOnly = 2,
    /// `Z_RLE` (3) -- match distances limited to one, i.e. run-length encoding.
    Rle = 3,
    /// `Z_FIXED` (4) -- normal matching, but dynamic Huffman codes forbidden.
    Fixed = 4,
}

impl Strategy {
    /// All five strategies, in the order their C values run.
    ///
    /// Provided because the differential test matrix enumerates every strategy
    /// against every level, `windowBits` form, `memLevel` and flush mode, and an
    /// exhaustive list that lives beside the enum cannot fall out of step with it.
    ///
    /// Mirrors `zlib.h` L200-L204.
    pub const ALL: [Self; 5] = [
        Self::Default,
        Self::Filtered,
        Self::HuffmanOnly,
        Self::Rle,
        Self::Fixed,
    ];

    /// Returns the C `int` that names this strategy.
    ///
    /// Mirrors `zlib.h` L200-L204.
    #[must_use]
    pub const fn as_raw(self) -> i32 {
        match self {
            Self::Default => Z_DEFAULT_STRATEGY,
            Self::Filtered => Z_FILTERED,
            Self::HuffmanOnly => Z_HUFFMAN_ONLY,
            Self::Rle => Z_RLE,
            Self::Fixed => Z_FIXED,
        }
    }

    /// Converts a raw `strategy` argument into a [`Strategy`], returning [`None`]
    /// for any value outside `Z_DEFAULT_STRATEGY ..= Z_FIXED`.
    ///
    /// This is the `strategy < 0 || strategy > Z_FIXED` term that both
    /// `deflateInit2_` (`deflate.c` L436) and `deflateParams` (`deflate.c` L786)
    /// test, expressed as a total function: the guard is a range over the five
    /// declared values, which is the same predicate as the two comparisons
    /// because the values are contiguous from zero.
    ///
    /// Mirrors `deflate.c` L436 and L786.
    #[must_use]
    pub const fn from_raw(raw: i32) -> Option<Self> {
        match raw {
            Z_DEFAULT_STRATEGY => Some(Self::Default),
            Z_FILTERED => Some(Self::Filtered),
            Z_HUFFMAN_ONLY => Some(Self::HuffmanOnly),
            Z_RLE => Some(Self::Rle),
            Z_FIXED => Some(Self::Fixed),
            _ => None,
        }
    }

    /// The reference spelling of this strategy, for diagnostics.
    ///
    /// Mirrors `zlib.h` L200-L204.
    #[must_use]
    pub const fn c_name(self) -> &'static str {
        match self {
            Self::Default => "Z_DEFAULT_STRATEGY",
            Self::Filtered => "Z_FILTERED",
            Self::HuffmanOnly => "Z_HUFFMAN_ONLY",
            Self::Rle => "Z_RLE",
            Self::Fixed => "Z_FIXED",
        }
    }
}

impl TryFrom<i32> for Strategy {
    type Error = ReturnCode;

    /// Converts a raw `strategy` argument into a [`Strategy`].
    ///
    /// The fallible-conversion form of [`Strategy::from_raw`], for call sites
    /// that propagate a status code with `?`.
    ///
    /// # Errors
    ///
    /// Returns [`ReturnCode::STREAM_ERROR`] for any value outside
    /// `Z_DEFAULT_STRATEGY ..= Z_FIXED`, which is the code `deflateInit2_` and
    /// `deflateParams` return for an invalid strategy (`deflate.c` L436-L437 and
    /// L786-L788).
    fn try_from(raw: i32) -> Result<Self, Self::Error> {
        Self::from_raw(raw).ok_or(ReturnCode::STREAM_ERROR)
    }
}

/// A flush request: the second argument of `deflate` and `inflate`.
///
/// The seven values of `zlib.h` L172-L178, complete and in order. The type lives
/// here rather than in `deflate/mod.rs` because both engines take a flush
/// argument and the differential matrix enumerates flush modes alongside the
/// other parameters, so this is the one place all three can agree.
///
/// # The two engines disagree about which values are legal
///
/// This is not symmetric and must not be made symmetric:
///
/// * `deflate` validates the argument and rejects anything outside
///   `Z_NO_FLUSH ..= Z_BLOCK`: `flush > Z_BLOCK || flush < 0` (`deflate.c`
///   L985). [`Z_TREES`] is therefore **not** a legal flush for compression, which
///   is what [`Flush::is_valid_for_deflate`] records and what
///   [`validate_deflate_flush`] enforces.
/// * `inflate` does not validate the argument at all. It tests only for the
///   specific values it treats specially -- `flush == Z_BLOCK`,
///   `flush == Z_TREES` and `flush != Z_FINISH` -- so any other integer behaves
///   exactly like [`Z_NO_FLUSH`], including integers that name no flush mode.
///   Rejecting an unknown flush in `inflate` would refuse calls the
///   reference has always accepted, so `inflate` must **not** be given a
///   validating conversion; [`Flush::from_raw`] returning [`None`] means "not one
///   of the seven documented values", not "invalid input".
///
/// Mirrors `zlib.h` L172-L179.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Flush {
    /// `Z_NO_FLUSH` (0) -- accumulate input; emit when the encoder sees fit.
    #[default]
    NoFlush = 0,
    /// `Z_PARTIAL_FLUSH` (1) -- flush without aligning to a byte boundary.
    PartialFlush = 1,
    /// `Z_SYNC_FLUSH` (2) -- flush and align with an empty stored block.
    SyncFlush = 2,
    /// `Z_FULL_FLUSH` (3) -- as [`SyncFlush`](Self::SyncFlush), and reset the
    /// compression state so decompression can restart here.
    FullFlush = 3,
    /// `Z_FINISH` (4) -- no further input; finish the stream.
    Finish = 4,
    /// `Z_BLOCK` (5) -- stop at a deflate block boundary.
    Block = 5,
    /// `Z_TREES` (6) -- stop after a block header. Decompression only.
    Trees = 6,
}

impl Flush {
    /// All seven flush values, in the order their C values run.
    ///
    /// Mirrors `zlib.h` L172-L178.
    pub const ALL: [Self; 7] = [
        Self::NoFlush,
        Self::PartialFlush,
        Self::SyncFlush,
        Self::FullFlush,
        Self::Finish,
        Self::Block,
        Self::Trees,
    ];

    /// Returns the C `int` that names this flush mode.
    ///
    /// Mirrors `zlib.h` L172-L178.
    #[must_use]
    pub const fn as_raw(self) -> i32 {
        match self {
            Self::NoFlush => Z_NO_FLUSH,
            Self::PartialFlush => Z_PARTIAL_FLUSH,
            Self::SyncFlush => Z_SYNC_FLUSH,
            Self::FullFlush => Z_FULL_FLUSH,
            Self::Finish => Z_FINISH,
            Self::Block => Z_BLOCK,
            Self::Trees => Z_TREES,
        }
    }

    /// Converts a raw `flush` argument into a [`Flush`], returning [`None`] for
    /// any integer that names none of the seven documented modes.
    ///
    /// Total over `i32`. See the type-level note on why [`None`] must not be
    /// treated as an error by `inflate`.
    ///
    /// Mirrors `zlib.h` L172-L178.
    #[must_use]
    pub const fn from_raw(raw: i32) -> Option<Self> {
        match raw {
            Z_NO_FLUSH => Some(Self::NoFlush),
            Z_PARTIAL_FLUSH => Some(Self::PartialFlush),
            Z_SYNC_FLUSH => Some(Self::SyncFlush),
            Z_FULL_FLUSH => Some(Self::FullFlush),
            Z_FINISH => Some(Self::Finish),
            Z_BLOCK => Some(Self::Block),
            Z_TREES => Some(Self::Trees),
            _ => None,
        }
    }

    /// Whether `deflate` accepts this flush mode.
    ///
    /// True for `Z_NO_FLUSH ..= Z_BLOCK` and false for [`Trees`](Self::Trees),
    /// reproducing the `flush > Z_BLOCK` half of the guard at `deflate.c` L985.
    ///
    /// Mirrors `deflate.c` L985.
    #[must_use]
    pub const fn is_valid_for_deflate(self) -> bool {
        self.as_raw() <= Z_BLOCK
    }

    /// The reference spelling of this flush mode, for diagnostics.
    ///
    /// Mirrors `zlib.h` L172-L178.
    #[must_use]
    pub const fn c_name(self) -> &'static str {
        match self {
            Self::NoFlush => "Z_NO_FLUSH",
            Self::PartialFlush => "Z_PARTIAL_FLUSH",
            Self::SyncFlush => "Z_SYNC_FLUSH",
            Self::FullFlush => "Z_FULL_FLUSH",
            Self::Finish => "Z_FINISH",
            Self::Block => "Z_BLOCK",
            Self::Trees => "Z_TREES",
        }
    }
}

impl TryFrom<i32> for Flush {
    type Error = ReturnCode;

    /// Converts a raw `flush` argument into a [`Flush`].
    ///
    /// Note that this rejects only integers that name no documented mode; it does
    /// **not** apply `deflate`'s narrower rule. Use [`validate_deflate_flush`]
    /// on the compression path.
    ///
    /// # Errors
    ///
    /// Returns [`ReturnCode::STREAM_ERROR`] for any integer outside
    /// `Z_NO_FLUSH ..= Z_TREES`.
    fn try_from(raw: i32) -> Result<Self, Self::Error> {
        Self::from_raw(raw).ok_or(ReturnCode::STREAM_ERROR)
    }
}

/// The container a compressed stream is wrapped in.
///
/// This is the *classification* half of what a `windowBits` argument encodes; the
/// other half is the window exponent. [`decode_deflate_window_bits`] separates
/// the two.
///
/// # The numeric encoding is load-bearing
///
/// `deflate_state.wrap` holds this classification as a plain `int` with the
/// values `0` for raw, `1` for zlib and `2` for gzip, and code well outside
/// configuration depends on exactly those three numbers:
///
/// * `read_buf` dispatches the running check value on them -- `wrap == 1`
///   accumulates an Adler-32 and `wrap == 2` accumulates a CRC-32 (`deflate.c`
///   L228 and L232). [`Wrap::computes_adler32`] and [`Wrap::computes_crc32`] are
///   that dispatch, so `read_buf.rs` can ask the question instead of comparing
///   integers.
/// * `deflateInit2_` rejects a 256-byte window unless `wrap == 1`, because only
///   the zlib header can transmit a window size to the decompressor (`deflate.c`
///   L436).
/// * `deflate` selects the header and trailer to emit on the same field
///   (`deflate.c` L664-L669 and L1264-L1268), and `deflateSetDictionary` clears it
///   to zero to stop the Adler-32 accumulating (`deflate.c` L577).
///
/// [`Wrap::as_deflate_wrap`] is therefore part of this module's contract, not a
/// convenience: `deflate/state.rs` stores what it returns.
///
/// Inflate's `wrap` field is a *different* encoding -- a bitmask that also has to
/// express "either header" and "verify the check value" -- so it has its own type,
/// [`InflateWrap`]. The two must not be interchanged.
///
/// Mirrors `deflate.c` L391 (`int wrap = 1`), L422-L432 and L228-L232.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wrap {
    /// Raw deflate: no header, no trailer and no check value, as RFC 1951
    /// describes on its own. Requested with a negative `windowBits`
    /// (`zlib.h` L570-L572).
    None,
    /// The zlib container of RFC 1950: a two-byte header, and an Adler-32
    /// trailer. The default, and the only container that transmits the window
    /// size to the decompressor.
    Zlib,
    /// The gzip container of RFC 1952: a ten-byte header, and a CRC-32 plus
    /// length trailer. Requested by adding 16 to `windowBits`
    /// (`zlib.h` L574-L580).
    Gzip,
}

impl Wrap {
    /// All three containers a deflate stream can be written in.
    ///
    /// Mirrors `deflate.c` L391, L423 and L430.
    pub const ALL: [Self; 3] = [Self::None, Self::Zlib, Self::Gzip];

    /// Returns the integer `deflate_state.wrap` holds for this container: `0`
    /// raw, `1` zlib, `2` gzip.
    ///
    /// Mirrors `deflate.c` L391, L423 and L430.
    #[must_use]
    pub const fn as_deflate_wrap(self) -> i32 {
        match self {
            Self::None => 0,
            Self::Zlib => 1,
            Self::Gzip => 2,
        }
    }

    /// Converts a raw `deflate_state.wrap` value back into a container,
    /// returning [`None`] for anything but `0`, `1` or `2`.
    ///
    /// Mirrors `deflate.c` L391, L423 and L430.
    #[must_use]
    pub const fn from_deflate_wrap(raw: i32) -> Option<Self> {
        match raw {
            0 => Some(Self::None),
            1 => Some(Self::Zlib),
            2 => Some(Self::Gzip),
            _ => None,
        }
    }

    /// Whether the compressor accumulates an Adler-32 over the input it reads.
    ///
    /// True for [`Zlib`](Self::Zlib) only, reproducing `wrap == 1` at
    /// `deflate.c` L228.
    ///
    /// Mirrors `deflate.c` L228.
    #[must_use]
    pub const fn computes_adler32(self) -> bool {
        matches!(self, Self::Zlib)
    }

    /// Whether the compressor accumulates a CRC-32 over the input it reads.
    ///
    /// True for [`Gzip`](Self::Gzip) only, reproducing `wrap == 2` at
    /// `deflate.c` L232.
    ///
    /// Mirrors `deflate.c` L232.
    #[must_use]
    pub const fn computes_crc32(self) -> bool {
        matches!(self, Self::Gzip)
    }

    /// Whether a header and trailer are written at all.
    ///
    /// False for [`None`](Self::None) only. `deflate.c` L1264 uses the equivalent
    /// test, `s->wrap <= 0`, to skip the trailer of a raw stream.
    ///
    /// Mirrors `deflate.c` L1264.
    #[must_use]
    pub const fn is_wrapped(self) -> bool {
        !matches!(self, Self::None)
    }
}

/// Which containers a decompressor will accept.
///
/// Inflate needs one more case than [`Wrap`] does, because a decoder may be asked
/// to work out for itself which container it has been handed:
/// [`ZlibOrGzip`](Self::ZlibOrGzip) has no compression counterpart.
///
/// # The numeric encoding is a bitmask, not a tag
///
/// `inflate_state.wrap` is not the small enumeration `deflate_state.wrap` is. It
/// is a set of flags, built by `wrap = (windowBits >> 4) + 5` (`inflate.c` L152):
///
/// | Requested `windowBits` | `wrap` | Bits |
/// |---|---|---|
/// | negative (raw) | 0 | none |
/// | `0 ..= 15` | 5 | zlib header, verify check |
/// | `16 ..= 31` | 6 | gzip header, verify check |
/// | `32 ..= 47` | 7 | either header, verify check |
///
/// The individual bits are [`INFLATE_WRAP_ZLIB_HEADER`],
/// [`INFLATE_WRAP_GZIP_HEADER`] and [`INFLATE_WRAP_VERIFY_CHECK`], and the whole
/// value is what [`InflateWrap::as_inflate_wrap`] returns for
/// `inflate/state.rs` to store.
///
/// Two later mutations of that field are state transitions rather than
/// configuration, and so are deliberately not modelled here: `inflateValidate`
/// clears the check-value bit (`inflate.c` L1390-L1393), and `inflateSync` zeroes
/// the field entirely when no header has been seen yet, treating the remainder as
/// raw (`inflate.c` L1300-L1302).
///
/// Mirrors `inflate.c` L145-L157.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InflateWrap {
    /// Raw inflate: process deflate data with no header, no trailer and no check
    /// value (`zlib.h` L878-L888).
    None,
    /// Accept a zlib header only; a gzip stream is a `Z_DATA_ERROR`.
    Zlib,
    /// Accept a gzip header only; a zlib stream is a `Z_DATA_ERROR`
    /// (`zlib.h` L890-L893).
    Gzip,
    /// Accept either, deciding from the first two bytes of the stream: the
    /// automatic header detection `windowBits + 32` asks for
    /// (`zlib.h` L890-L892).
    ZlibOrGzip,
}

impl InflateWrap {
    /// Every wrap request a decompressor can be initialised with.
    ///
    /// Mirrors `inflate.c` L148 and L152.
    pub const ALL: [Self; 4] = [Self::None, Self::Zlib, Self::Gzip, Self::ZlibOrGzip];

    /// Returns the integer `inflate_state.wrap` holds for this request: `0`, `5`,
    /// `6` or `7`.
    ///
    /// Mirrors `inflate.c` L148 and L152.
    #[must_use]
    pub const fn as_inflate_wrap(self) -> i32 {
        match self {
            Self::None => INFLATE_WRAP_RAW,
            Self::Zlib => INFLATE_WRAP_ZLIB_ONLY,
            Self::Gzip => INFLATE_WRAP_GZIP_ONLY,
            Self::ZlibOrGzip => INFLATE_WRAP_AUTODETECT,
        }
    }

    /// Converts a raw `inflate_state.wrap` value back into a request, returning
    /// [`None`] for anything but `0`, `5`, `6` or `7`.
    ///
    /// Only the four values `inflateReset2` can produce are accepted. The
    /// check-value bit that `inflateValidate` may later clear would make `1`, `2`
    /// and `3` reachable in a live state, and those are deliberately *not*
    /// accepted here: this converts a configuration request, not an arbitrary
    /// snapshot of a running stream.
    ///
    /// Mirrors `inflate.c` L148 and L152.
    #[must_use]
    pub const fn from_inflate_wrap(raw: i32) -> Option<Self> {
        match raw {
            INFLATE_WRAP_RAW => Some(Self::None),
            INFLATE_WRAP_ZLIB_ONLY => Some(Self::Zlib),
            INFLATE_WRAP_GZIP_ONLY => Some(Self::Gzip),
            INFLATE_WRAP_AUTODETECT => Some(Self::ZlibOrGzip),
            _ => None,
        }
    }

    /// Whether a zlib header may be read.
    ///
    /// Reproduces `state->wrap & 1` at `inflate.c` L524.
    ///
    /// Mirrors `inflate.c` L524.
    #[must_use]
    pub const fn allows_zlib_header(self) -> bool {
        matches!(self, Self::Zlib | Self::ZlibOrGzip)
    }

    /// Whether a gzip header may be read.
    ///
    /// Reproduces `state->wrap & 2` at `inflate.c` L513, which is also the test
    /// `inflateGetHeader` applies before agreeing to fill a caller's `gz_header`
    /// (`inflate.c` L1225).
    ///
    /// Mirrors `inflate.c` L513.
    #[must_use]
    pub const fn allows_gzip_header(self) -> bool {
        matches!(self, Self::Gzip | Self::ZlibOrGzip)
    }

    /// Whether the stream's trailing check value is verified, as initialised.
    ///
    /// True for every wrapped request, because the `+ 5` bias of `inflate.c` L152
    /// sets [`INFLATE_WRAP_VERIFY_CHECK`] in all three of them, and false for
    /// [`None`](Self::None). A later `inflateValidate(strm, 0)` can clear the bit
    /// on a live stream; that is `inflate/state.rs`'s concern.
    ///
    /// Mirrors `inflate.c` L152 and L1390-L1393.
    #[must_use]
    pub const fn verifies_check_value(self) -> bool {
        !matches!(self, Self::None)
    }

    /// The container this request pins down, or [`None`] when it does not pin one
    /// down.
    ///
    /// [`ZlibOrGzip`](Self::ZlibOrGzip) is the case that yields [`None`]: which
    /// container is in play is not known until the first two bytes of the stream
    /// have been read (`inflate.c` L513), so there is no honest [`Wrap`] to
    /// report for it yet.
    #[must_use]
    pub const fn determinate_wrap(self) -> Option<Wrap> {
        match self {
            Self::None => Some(Wrap::None),
            Self::Zlib => Some(Wrap::Zlib),
            Self::Gzip => Some(Wrap::Gzip),
            Self::ZlibOrGzip => None,
        }
    }
}

/// Decodes a `deflateInit2_` `windowBits` argument into a container and a window
/// exponent.
///
/// This is the compression half of the parameter that means two things at once,
/// and it is a literal mirror of `deflate.c` L422-L439 -- decode, then validate,
/// then promote:
///
/// | Argument | Result |
/// |---|---|
/// | `8` | `(Wrap::Zlib, 9)` -- promoted, see below |
/// | `9 ..= 15` | `(Wrap::Zlib, 9 ..= 15)` |
/// | `-15 ..= -9` | `(Wrap::None, 9 ..= 15)` |
/// | `25 ..= 31` | `(Wrap::Gzip, 9 ..= 15)` |
/// | anything else | `Err(Z_STREAM_ERROR)` |
///
/// Three rejections in that last row are worth stating explicitly, because each
/// one is a case a plausible implementation would accept:
///
/// * `-8` and `24` are refused. Both decode to a 256-byte window, and
///   `deflate.c` L436 refuses a 256-byte window for anything but a zlib stream:
///   only the zlib header can tell the decompressor how large the window was
///   (`zlib.h` L582-L584).
/// * `0` is refused. It is inflate's "take the size from the header" request and
///   means nothing to a compressor, so it falls through to the `windowBits < 8`
///   test.
/// * `16` and `32 ..= 47` are refused. `16` decodes to a zero exponent, and
///   anything from `32` up decodes to an exponent above 15; inflate's `+32`
///   automatic-detection request has no compression counterpart.
///
/// The returned exponent is never 8: a request for `8` is promoted to `9` after
/// validation, exactly as `deflate.c` L439 does, because the current encoder
/// cannot work with a 256-byte window. `zlib.h` L562-L568 documents the
/// consequence for callers -- a stream compressed that way carries `9` in its
/// header, so passing `8` to `inflateInit2_` for it is an error.
///
/// Every arithmetic step is checked, so no argument -- including [`i32::MIN`],
/// whose negation is not representable, and [`i32::MAX`] -- can overflow or
/// panic.
///
/// # Errors
///
/// Returns [`ReturnCode::STREAM_ERROR`] for every argument outside the table
/// above, which is the single code `deflateInit2_` returns for a bad parameter
/// (`deflate.c` L437).
///
/// Mirrors `deflate.c` L422-L439.
pub fn decode_deflate_window_bits(window_bits: i32) -> Result<(Wrap, u8), ReturnCode> {
    let (wrap, exponent) = if window_bits < 0 {
        // `deflate.c` L422-L427: a negative request suppresses the zlib wrapper,
        // and its magnitude is the window exponent. The reference rejects
        // anything below -15 before negating, which is also what keeps the
        // negation below in range; `checked_neg` makes that structural rather
        // than argued, since `i32::MIN` has no positive counterpart.
        if window_bits < -MAX_WBITS {
            return Err(ReturnCode::STREAM_ERROR);
        }
        let magnitude = window_bits.checked_neg().ok_or(ReturnCode::STREAM_ERROR)?;
        (Wrap::None, magnitude)
    } else if window_bits > MAX_WBITS {
        // `deflate.c` L429-L432: 16 or more selects the gzip wrapper, and the
        // exponent is what remains once the offset is taken back off. Guarded by
        // `#ifdef GZIP`, which `deflate.h` L22-L23 defines by default.
        let exponent = window_bits
            .checked_sub(GZIP_WRAP_OFFSET)
            .ok_or(ReturnCode::STREAM_ERROR)?;
        (Wrap::Gzip, exponent)
    } else {
        // The zlib container: the argument is the exponent, unadjusted.
        (Wrap::Zlib, window_bits)
    };

    // `deflate.c` L434-L438, the `windowBits` terms of the single validation
    // condition. The range is written with `contains` rather than as the two
    // comparisons `windowBits < 8 || windowBits > 15`; it is the same predicate.
    if !(MIN_WBITS..=MAX_WBITS).contains(&exponent) {
        return Err(ReturnCode::STREAM_ERROR);
    }
    // `deflate.c` L436, `(windowBits == 8 && wrap != 1)`.
    if exponent == MIN_WBITS && wrap != Wrap::Zlib {
        return Err(ReturnCode::STREAM_ERROR);
    }

    // `deflate.c` L439, "until 256-byte window bug fixed".
    let promoted = if exponent == MIN_WBITS {
        MIN_WBITS + 1
    } else {
        exponent
    };

    // In range `9..=15` by the two checks above, so the conversion cannot fail;
    // it is still fallible rather than a cast so that the impossibility is
    // enforced by the compiler instead of by this comment.
    let promoted = u8::try_from(promoted).map_err(|_| ReturnCode::STREAM_ERROR)?;
    Ok((wrap, promoted))
}

/// Decodes an `inflateInit2_` (or `inflateReset2`) `windowBits` argument into a
/// wrap request and a window exponent.
///
/// The decompression half of the parameter, and a literal mirror of `inflate.c`
/// L145-L161. It is **not** the same function as
/// [`decode_deflate_window_bits`]:
///
/// | Argument | Result |
/// |---|---|
/// | `0` | `(InflateWrap::Zlib, 0)` -- window size comes from the header |
/// | `8 ..= 15` | `(InflateWrap::Zlib, 8 ..= 15)` |
/// | `-15 ..= -8` | `(InflateWrap::None, 8 ..= 15)` |
/// | `16` | `(InflateWrap::Gzip, 0)` |
/// | `24 ..= 31` | `(InflateWrap::Gzip, 8 ..= 15)` |
/// | `32` | `(InflateWrap::ZlibOrGzip, 0)` |
/// | `40 ..= 47` | `(InflateWrap::ZlibOrGzip, 8 ..= 15)` |
/// | anything else | `Err(Z_STREAM_ERROR)` |
///
/// The four rows that have no deflate analogue, and the one that differs:
///
/// * A returned exponent of `0` means "use the window size recorded in the
///   stream header" (`zlib.h` L875-L876). It survives the range check because
///   the reference exempts it: `if (windowBits && (windowBits < 8 || windowBits >
///   15))` (`inflate.c` L160). Resolve it once a header has been read, with
///   [`ValidatedInflateConfig::resolve_zlib_header_window_bits`] or
///   [`ValidatedInflateConfig::resolve_gzip_window_bits`].
/// * `16 ..= 31` asks for gzip only and `32 ..= 47` for automatic detection,
///   both of which fall out of `wrap = (windowBits >> 4) + 5` (`inflate.c`
///   L152).
/// * `-8` is accepted, where deflate refuses it: a decompressor is told the
///   window size, so a 256-byte raw window is unambiguous.
/// * `48` and above are refused. The reference masks the exponent out of the low
///   nibble only for requests below 48 (`inflate.c` L154-L155), so a larger
///   request keeps its full value and fails the range check -- which is how
///   `wrap` can never exceed 7.
///
/// Every arithmetic step is checked, so no argument -- [`i32::MIN`] and
/// [`i32::MAX`] included -- can overflow or panic.
///
/// # Errors
///
/// Returns [`ReturnCode::STREAM_ERROR`] for every argument outside the table
/// above, which is the code `inflateReset2` returns (`inflate.c` L147 and L161).
///
/// Mirrors `inflate.c` L145-L161.
pub fn decode_inflate_window_bits(window_bits: i32) -> Result<(InflateWrap, u8), ReturnCode> {
    let (wrap_request, exponent) = if window_bits < 0 {
        // `inflate.c` L145-L150. As on the deflate side, the sub-range check runs
        // before the negation, and `checked_neg` keeps `i32::MIN` structurally
        // impossible rather than merely unreachable.
        if window_bits < -MAX_WBITS {
            return Err(ReturnCode::STREAM_ERROR);
        }
        let magnitude = window_bits.checked_neg().ok_or(ReturnCode::STREAM_ERROR)?;
        (INFLATE_WRAP_RAW, magnitude)
    } else {
        // `inflate.c` L152: the wrap request is the argument's high bits, biased
        // so that a plain `0..=15` request still asks for a zlib header and a
        // verified check value. The addition cannot overflow -- the shifted term
        // is at most `i32::MAX >> 4` -- and is checked regardless.
        let request = (window_bits >> INFLATE_WRAP_SHIFT)
            .checked_add(INFLATE_WRAP_BIAS)
            .ok_or(ReturnCode::STREAM_ERROR)?;
        // `inflate.c` L154-L155: the exponent lives in the low nibble, but only
        // for requests below 48. Guarded by `#ifdef GUNZIP`, which `inflate.h`
        // L15-L16 defines by default.
        let exponent = if window_bits < INFLATE_WRAP_MASK_LIMIT {
            window_bits & INFLATE_WINDOW_BITS_MASK
        } else {
            window_bits
        };
        (request, exponent)
    };

    // `inflate.c` L160-L161. Zero is exempt, and that exemption is the whole of
    // the "take the window size from the header" feature.
    if exponent != WINDOW_BITS_FROM_HEADER && !(MIN_WBITS..=MAX_WBITS).contains(&exponent) {
        return Err(ReturnCode::STREAM_ERROR);
    }

    // Only 0, 5, 6 and 7 can reach this point: a request of 48 or more keeps its
    // full value above and has already been rejected by the range check, so the
    // shifted term is at most 2. The fallible conversion states that rather than
    // assuming it.
    let wrap = InflateWrap::from_inflate_wrap(wrap_request).ok_or(ReturnCode::STREAM_ERROR)?;
    // Either 0 or `8..=15`, so this cannot fail either.
    let exponent = u8::try_from(exponent).map_err(|_| ReturnCode::STREAM_ERROR)?;
    Ok((wrap, exponent))
}

/// Validates an `inflateBackInit_` `windowBits` argument.
///
/// `inflateBack` is the strictest of the three entry points, and deliberately so:
/// the caller supplies the window buffer itself, so the library cannot pick a
/// size or grow one later. `infback.c` L22-L23 states the contract -- "windowBits
/// is in the range 8..15, and window is a user-supplied window and output buffer
/// that is 2**windowBits bytes" -- and `infback.c` L33-L35 enforces it with a
/// plain `windowBits < 8 || windowBits > 15`.
///
/// So none of `inflateInit2_`'s conveniences apply here: a negative argument, a
/// `+16` gzip request, a `+32` automatic-detection request and a `0`
/// "size from the header" request are all rejected. `inflateBack` decodes raw
/// deflate data unconditionally, which is why there is no container to select and
/// nothing for this function to return but the exponent.
///
/// # Errors
///
/// Returns [`ReturnCode::STREAM_ERROR`] for any argument outside `8 ..= 15`,
/// which is the code `inflateBackInit_` returns (`infback.c` L35).
///
/// Mirrors `infback.c` L33-L35.
pub fn validate_inflate_back_window_bits(window_bits: i32) -> Result<u8, ReturnCode> {
    if !(MIN_WBITS..=MAX_WBITS).contains(&window_bits) {
        return Err(ReturnCode::STREAM_ERROR);
    }
    u8::try_from(window_bits).map_err(|_| ReturnCode::STREAM_ERROR)
}

/// Resolves a `level` argument to the level the encoder will actually use.
///
/// Two steps, in this order, because the order is what makes `-1` legal and `-2`
/// not:
///
/// 1. [`Z_DEFAULT_COMPRESSION`] becomes [`DEF_LEVEL`] (`deflate.c` L419).
/// 2. The result must lie in `0 ..= 9` (`deflate.c` L435).
///
/// Swapping them would reject [`Z_DEFAULT_COMPRESSION`], which every caller of
/// `deflateInit` relies on (`deflate.c` L381 passes the caller's level straight
/// through, and `compress` passes `Z_DEFAULT_COMPRESSION` itself).
///
/// The `FASTEST` build of the reference replaces step 1 with `if (level != 0)
/// level = 1` (`deflate.c` L417), which changes the emitted bytes. The implementation
/// implements the default build only, so that variant is absent by design.
///
/// # Errors
///
/// Returns [`ReturnCode::STREAM_ERROR`] for any level outside
/// `0 ..= 9` once the default has been resolved -- `-2` and `10` being the two
/// nearest failures.
///
/// Mirrors `deflate.c` L419 and L435.
pub fn normalize_deflate_level(level: i32) -> Result<u8, ReturnCode> {
    // `deflate.c` L419.
    let resolved = if level == Z_DEFAULT_COMPRESSION {
        DEF_LEVEL
    } else {
        level
    };

    // `deflate.c` L435, `level < 0 || level > 9`.
    if !(Z_NO_COMPRESSION..=Z_BEST_COMPRESSION).contains(&resolved) {
        return Err(ReturnCode::STREAM_ERROR);
    }

    u8::try_from(resolved).map_err(|_| ReturnCode::STREAM_ERROR)
}

/// Validates a `memLevel` argument.
///
/// The accepted range is `1 ..= MAX_MEM_LEVEL`, and [`MAX_MEM_LEVEL`] is **9**
/// (`zconf.h` L272-L279) -- not the 8 of [`DEF_MEM_LEVEL`]. `memLevel` sizes the
/// hash table and the symbol buffer, `hash_bits = memLevel + 7` and
/// `lit_bufsize = 1 << (memLevel + 6)` (`deflate.c` L453 and L464), so it changes
/// how much memory a stream needs and how well it compresses, but never the
/// format of what it emits.
///
/// # Errors
///
/// Returns [`ReturnCode::STREAM_ERROR`] for any value outside `1 ..= 9`, `0` and
/// `10` being the two nearest failures.
///
/// Mirrors the `memLevel < 1 || memLevel > MAX_MEM_LEVEL` term of `deflate.c`
/// L434.
pub fn validate_mem_level(mem_level: i32) -> Result<u8, ReturnCode> {
    if !(MIN_MEM_LEVEL..=MAX_MEM_LEVEL).contains(&mem_level) {
        return Err(ReturnCode::STREAM_ERROR);
    }
    u8::try_from(mem_level).map_err(|_| ReturnCode::STREAM_ERROR)
}

/// Validates the `level` and `strategy` arguments of `deflateParams`.
///
/// `deflateParams` re-tunes a live stream, and it applies exactly the bounds
/// `deflateInit2_` applies to the same two parameters -- the default-level
/// resolution included (`deflate.c` L784-L788). It exists as its own function so
/// that `deflate/mod.rs` does not have to reach for two separate validators and
/// risk applying only one of them.
///
/// What this deliberately does *not* cover is the rest of `deflateParams`'s
/// behaviour: flushing the pending block when the strategy or the compression
/// function changes, and the `Z_BUF_ERROR` that follows if input remains
/// (`deflate.c` L791-L799). Those are stream operations, not parameter checks.
///
/// # Errors
///
/// Returns [`ReturnCode::STREAM_ERROR`] if the level is outside `0 ..= 9` after
/// [`Z_DEFAULT_COMPRESSION`] has been resolved, or the strategy names none of the
/// five documented values.
///
/// Mirrors `deflate.c` L784-L788.
pub fn validate_deflate_params_change(
    level: i32,
    strategy: i32,
) -> Result<(u8, Strategy), ReturnCode> {
    let level = normalize_deflate_level(level)?;
    let strategy = Strategy::try_from(strategy)?;
    Ok((level, strategy))
}

/// Validates a `flush` argument for `deflate`.
///
/// `deflate` is the only entry point that checks its flush argument:
/// `flush > Z_BLOCK || flush < 0` (`deflate.c` L985). Both halves are reproduced
/// here -- [`Flush::from_raw`] rejects anything that names no mode at all, and
/// [`Flush::is_valid_for_deflate`] rejects [`Flush::Trees`], which is a
/// decompression-only request.
///
/// There is deliberately no counterpart for `inflate`, which accepts any `int`;
/// see the note on [`Flush`].
///
/// # Errors
///
/// Returns [`ReturnCode::STREAM_ERROR`] for a negative argument, for anything
/// above [`Z_BLOCK`], and for [`Z_TREES`] specifically.
///
/// Mirrors `deflate.c` L985.
pub fn validate_deflate_flush(flush: i32) -> Result<Flush, ReturnCode> {
    let flush = Flush::from_raw(flush).ok_or(ReturnCode::STREAM_ERROR)?;
    if !flush.is_valid_for_deflate() {
        return Err(ReturnCode::STREAM_ERROR);
    }
    Ok(flush)
}

/// The five parameters of `deflateInit2_`, as requested by the caller.
///
/// This is the *request*, not the resolved configuration: `level` may still be
/// [`Z_DEFAULT_COMPRESSION`] and `window_bits` may still carry a container in its
/// sign or in a `+16` offset, exactly as the caller wrote them. Turn it into a
/// resolved configuration with [`DeflateConfig::validate`], which is where every
/// bound is applied.
///
/// `method` and `strategy` are typed while the three integers are not, and that
/// asymmetry is deliberate. A method or a strategy is a choice from a fixed set,
/// so an invalid one need never be representable. A level is a number on a scale
/// with one magic value, and a `window_bits` is a small encoded record; keeping
/// them as `i32` is what lets this type hold precisely what a C caller passed,
/// which is what makes it a faithful mirror of the C parameter list rather than a
/// re-interpretation of it.
///
/// Mirrors `deflateInit2_`, `zlib.h` L1907-L1910 and `deflate.c` L387.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeflateConfig {
    /// `0 ..= 9`, or [`Z_DEFAULT_COMPRESSION`] for [`DEF_LEVEL`].
    pub level: i32,
    /// The compression method; only [`Method::Deflated`] exists.
    pub method: Method,
    /// The window exponent, with the container encoded in it: `8 ..= 15` for
    /// zlib, `-15 ..= -9` for raw, `25 ..= 31` for gzip. See
    /// [`decode_deflate_window_bits`] for the exact rules, including the
    /// promotion of `8` to `9`.
    pub window_bits: i32,
    /// `1 ..= 9`; how much memory the internal state may use.
    pub mem_level: i32,
    /// How to trade string matching against Huffman coding.
    pub strategy: Strategy,
}

impl DeflateConfig {
    /// The configuration `deflateInit` builds for a given level.
    ///
    /// `deflateInit_` supplies [`Z_DEFLATED`], [`MAX_WBITS`], [`DEF_MEM_LEVEL`]
    /// and [`Z_DEFAULT_STRATEGY`] on the caller's behalf and forwards the level
    /// unchanged (`deflate.c` L379-L383), so this is that call with the same four
    /// defaults. It is also the configuration `compress` and `compress2` use,
    /// since both go through `deflateInit` (`compress.c` L42, reached from L80 and
    /// L84 with `Z_DEFAULT_COMPRESSION`).
    ///
    /// The level is not validated here, exactly as `deflateInit` does not
    /// validate it: `deflateInit2_` does, and so does
    /// [`DeflateConfig::validate`].
    #[must_use]
    pub const fn new(level: i32) -> Self {
        Self {
            level,
            method: Method::Deflated,
            window_bits: MAX_WBITS,
            mem_level: DEF_MEM_LEVEL,
            strategy: Strategy::Default,
        }
    }

    /// Builds a configuration from the five raw `int` arguments of
    /// `deflateInit2_`, typing `method` and `strategy` on the way in.
    ///
    /// For callers that want to hold a `DeflateConfig`; [`validate_deflate_params`]
    /// is the one-step form that goes straight to a resolved configuration.
    ///
    /// # Errors
    ///
    /// Returns [`ReturnCode::STREAM_ERROR`] if `method` is not [`Z_DEFLATED`] or
    /// `strategy` names none of the five documented values. The three integer
    /// parameters are checked by [`DeflateConfig::validate`], not here, and the
    /// code is the same either way (`deflate.c` L434-L437).
    pub fn from_raw(
        level: i32,
        method: i32,
        window_bits: i32,
        mem_level: i32,
        strategy: i32,
    ) -> Result<Self, ReturnCode> {
        Ok(Self {
            level,
            method: Method::try_from(method)?,
            window_bits,
            mem_level,
            strategy: Strategy::try_from(strategy)?,
        })
    }

    /// Applies every bound `deflateInit2_` applies, and resolves the defaults.
    ///
    /// # Errors
    ///
    /// Returns [`ReturnCode::STREAM_ERROR`] if any parameter is out of range; see
    /// [`validate_deflate_params`], which this delegates to.
    ///
    /// Mirrors `deflate.c` L419-L439.
    pub fn validate(self) -> Result<ValidatedDeflateConfig, ReturnCode> {
        validate_deflate_params(
            self.level,
            self.method.as_raw(),
            self.window_bits,
            self.mem_level,
            self.strategy.as_raw(),
        )
    }
}

impl Default for DeflateConfig {
    /// The configuration `deflateInit` builds: level [`Z_DEFAULT_COMPRESSION`],
    /// method [`Z_DEFLATED`], `windowBits` [`MAX_WBITS`], `memLevel`
    /// [`DEF_MEM_LEVEL`] and strategy [`Z_DEFAULT_STRATEGY`].
    ///
    /// Mirrors `deflate.c` L379-L383.
    fn default() -> Self {
        Self::new(Z_DEFAULT_COMPRESSION)
    }
}

/// A compression configuration that has passed every check `deflateInit2_`
/// applies, with the defaults resolved and the container separated out.
///
/// Each field is narrower than the request it came from, and the narrowing is the
/// point: `level` no longer carries the magic `-1`, `window_bits` no longer
/// carries the container, and the container is a [`Wrap`] rather than a sign and
/// an offset. [`validate_deflate_params`] and [`DeflateConfig::validate`] are the
/// two functions that *establish* the ranges documented on the fields below.
///
/// # The name is a provenance hint, not a proof -- read this before relying on it
///
/// **Every field is `pub`, and the type itself is `pub`, so any crate -- not just
/// this one -- can assemble an instance by hand with arbitrary values and never go
/// through validation at all.** The type is `Copy` with no private member, so
/// there is no seal to defeat. Do not read "Validated" as a compiler-enforced
/// invariant: it records where a value *normally* comes from, and nothing more.
///
/// The fields are public because `deflate/state.rs` reads them directly, which
/// keeps `DeflateState::new` free of accessor noise. The cost of that choice is
/// exactly the forgeability above, and it is stated here rather than glossed as
/// "trusted not to".
///
/// What follows from it, concretely:
///
/// * [`DeflateState::new`](crate::deflate::state::DeflateState::new) is the one
///   consumer that depends on these ranges -- it sizes the window, the hash
///   chains and the pending buffer from `window_bits` and `mem_level`. Reaching it
///   with a forged configuration is a caller error, not a memory-safety hole:
///   `#![forbid(unsafe_code)]` still holds, so the worst outcome is a wrong-sized
///   allocation, a rejected request, or a panic from a bounds check -- never
///   undefined behaviour.
/// * Anything that receives one of these from outside this crate should treat it
///   as an ordinary struct of five numbers. If a future consumer needs a value it
///   can actually trust, it must re-run [`validate_deflate_params`] itself.
///
/// Making the fields `pub(crate)` behind read-only accessors would convert the
/// hint into a guarantee, and it is the better design. It is not done here because
/// the C ABI facade that consumes this type has not landed yet, so narrowing the
/// surface now could break a consumer that cannot be inspected.
///
/// `deflate/state.rs` derives the rest of the state layout from these five
/// numbers, in the order `deflate.c` L447-L456 and L528-L530 do. The formula for the total
/// this costs is at `zconf.h` L290-L296: `(1 << (windowBits + 2)) + (1 <<
/// (memLevel + 9))` bytes, which is 128 KiB plus 128 KiB at the defaults, plus a
/// few kilobytes of small objects.
///
/// Mirrors the fields `deflateInit2_` assigns at `deflate.c` L447-L456 and
/// L528-L530.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ValidatedDeflateConfig {
    /// The compression level, `0 ..= 9`. Never [`Z_DEFAULT_COMPRESSION`]: that
    /// has already become [`DEF_LEVEL`].
    pub level: u8,
    /// The compression method.
    pub method: Method,
    /// The container to write. `deflate_state.wrap` stores
    /// [`Wrap::as_deflate_wrap`] of this.
    pub wrap: Wrap,
    /// The window exponent, `9 ..= 15`. Never 8: a request for 8 has already been
    /// promoted (`deflate.c` L439).
    pub window_bits: u8,
    /// The memory level, `1 ..= 9`.
    pub mem_level: u8,
    /// The strategy.
    pub strategy: Strategy,
}

/// Applies the whole of `deflateInit2_`'s parameter handling to five raw `int`
/// arguments.
///
/// This is the function the C ABI facade calls, and the only place the deflate
/// bounds are written down. It performs, in order: the default-level resolution
/// and level range check ([`normalize_deflate_level`]), the method check
/// ([`Method::try_from`]), the memory-level check ([`validate_mem_level`]), the
/// strategy check ([`Strategy::try_from`]) and the `windowBits` decode, range
/// check and promotion ([`decode_deflate_window_bits`]).
///
/// The reference tests all of those in one `||` chain and returns the same code
/// from every branch (`deflate.c` L434-L437), so checking them one at a time here
/// is observationally identical -- there is no ordering a caller can detect,
/// because there is only ever one status to see.
///
/// # Errors
///
/// Returns [`ReturnCode::STREAM_ERROR`] if any parameter is invalid: a level
/// outside `0 ..= 9` after [`Z_DEFAULT_COMPRESSION`] is resolved, a method other
/// than [`Z_DEFLATED`], a `memLevel` outside `1 ..= 9`, a strategy outside
/// `0 ..= 4`, or a `windowBits` that names no window (see
/// [`decode_deflate_window_bits`] for that table).
///
/// Mirrors `deflate.c` L419-L439.
pub fn validate_deflate_params(
    level: i32,
    method: i32,
    window_bits: i32,
    mem_level: i32,
    strategy: i32,
) -> Result<ValidatedDeflateConfig, ReturnCode> {
    let level = normalize_deflate_level(level)?;
    let method = Method::try_from(method)?;
    let mem_level = validate_mem_level(mem_level)?;
    let strategy = Strategy::try_from(strategy)?;
    let (wrap, window_bits) = decode_deflate_window_bits(window_bits)?;

    Ok(ValidatedDeflateConfig {
        level,
        method,
        wrap,
        window_bits,
        mem_level,
        strategy,
    })
}

/// The single parameter of `inflateInit2_`, as requested by the caller.
///
/// `window_bits` is held exactly as passed, container encoding and all; see
/// [`decode_inflate_window_bits`] for what the encoding means and
/// [`InflateConfig::validate`] for the checks.
///
/// Mirrors `inflateInit2_`, `zlib.h` L1911-L1912 and `inflate.c` L173.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InflateConfig {
    /// The window exponent with the wrap request encoded in it: `8 ..= 15` for
    /// zlib, `-15 ..= -8` for raw, `+16` for gzip only, `+32` for automatic
    /// detection, and `0` for "take the window size from the stream header".
    pub window_bits: i32,
}

impl InflateConfig {
    /// A configuration for the given raw `windowBits` request.
    ///
    /// The request is stored, not checked, exactly as `inflateInit2_` stores it
    /// before handing it to `inflateReset2` (`inflate.c` L173 and L206).
    #[must_use]
    pub const fn new(window_bits: i32) -> Self {
        Self { window_bits }
    }

    /// Applies the bounds `inflateReset2` applies, and separates the wrap request
    /// from the window exponent.
    ///
    /// # Errors
    ///
    /// Returns [`ReturnCode::STREAM_ERROR`] for a `windowBits` that names no
    /// window; see [`decode_inflate_window_bits`].
    ///
    /// Mirrors `inflate.c` L145-L161.
    pub fn validate(self) -> Result<ValidatedInflateConfig, ReturnCode> {
        let (wrap, window_bits) = decode_inflate_window_bits(self.window_bits)?;
        Ok(ValidatedInflateConfig { wrap, window_bits })
    }
}

impl Default for InflateConfig {
    /// The configuration `inflateInit` builds: `windowBits` [`DEF_WBITS`], which
    /// asks for a zlib stream with the largest window (`inflate.c` L216).
    ///
    /// `uncompress` and `uncompress2` use this configuration, since both go
    /// through `inflateInit` (`uncompr.c` L51).
    ///
    /// Mirrors `inflate.c` L214-L217.
    fn default() -> Self {
        Self::new(DEF_WBITS)
    }
}

/// A decompression configuration that has passed `inflateReset2`'s checks, with
/// the wrap request separated from the window exponent.
///
/// [`InflateConfig::validate`] is what *establishes* the ranges documented on the
/// two fields below. `inflate/state.rs` stores
/// [`InflateWrap::as_inflate_wrap`] of the wrap in its `wrap` field and the
/// exponent in `wbits`, as `inflate.c` L168-L169 does.
///
/// # The name is a provenance hint, not a proof
///
/// The same caveat that applies to [`ValidatedDeflateConfig`] applies here, for
/// the same reason: **both fields are `pub` and the type is `Copy` with no private
/// member, so any crate can build one by hand with arbitrary values without ever
/// calling [`InflateConfig::validate`].** "Validated" records where a value
/// normally comes from; it is not a compiler-enforced invariant.
///
/// [`InflateState::with_validated_config`](crate::inflate::state::InflateState::with_validated_config)
/// is the consumer that depends on the ranges, and it is where a forged value
/// would land. Because `#![forbid(unsafe_code)]` still holds, the worst outcome is
/// a wrong-sized window or a panic from a bounds check -- never undefined
/// behaviour. Any consumer outside this crate that needs a value it can trust must
/// re-run [`InflateConfig::validate`] itself. See
/// [`ValidatedDeflateConfig`] for why the fields are not `pub(crate)` today.
///
/// Ported from `inflate.c` L145-L169.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ValidatedInflateConfig {
    /// Which containers this stream will accept.
    pub wrap: InflateWrap,
    /// The window exponent: `8 ..= 15`, or `0` for "not decided yet -- read it
    /// from the stream header". Use [`window_size_from_header`] rather than
    /// comparing against zero at the use site.
    ///
    /// [`window_size_from_header`]: ValidatedInflateConfig::window_size_from_header
    pub window_bits: u8,
}

impl ValidatedInflateConfig {
    /// Whether the window size is still to be taken from the stream header.
    ///
    /// True exactly when [`window_bits`](Self::window_bits) is zero, which is the
    /// state `inflate.c` L514 and L540 both test for before filling it in.
    ///
    /// Mirrors `inflate.c` L540.
    #[must_use]
    pub const fn window_size_from_header(self) -> bool {
        self.window_bits == 0
    }

    /// Resolves the window exponent against the one advertised by a zlib header.
    ///
    /// `header_window_bits` is the header's own exponent, which `inflate` computes
    /// as `BITS(4) + 8` from the `CINFO` nibble and so may be anything in
    /// `8 ..= 23` (`inflate.c` L539). The rules, in the reference's order:
    ///
    /// 1. If no exponent was requested, adopt the header's (`inflate.c`
    ///    L540-L541).
    /// 2. Reject a header that advertises more than [`MAX_WBITS`], or more than
    ///    was requested (`inflate.c` L542).
    ///
    /// Rule 2 is the documented refusal to grow a window to fit a stream: "if a
    /// compressed stream with a larger window size is given as input, `inflate()`
    /// will return with the error code `Z_DATA_ERROR` instead of trying to allocate
    /// a larger window" (`zlib.h` L869-L873). Note that it is
    /// [`ReturnCode::DATA_ERROR`] and not
    /// [`ReturnCode::STREAM_ERROR`]: the parameters were fine, the *stream* is
    /// the problem, and the reference reports it by setting the message to
    /// "invalid window size" and entering the `BAD` state.
    ///
    /// # Errors
    ///
    /// Returns [`ReturnCode::DATA_ERROR`] when the header advertises a window
    /// larger than [`MAX_WBITS`] or larger than the one requested.
    ///
    /// Mirrors `inflate.c` L539-L546.
    pub fn resolve_zlib_header_window_bits(self, header_window_bits: u8) -> Result<u8, ReturnCode> {
        // `inflate.c` L540-L541.
        let effective = if self.window_size_from_header() {
            header_window_bits
        } else {
            self.window_bits
        };

        // `inflate.c` L542: `len > 15 || len > state->wbits`. The first
        // comparison is widened rather than narrowed so that a header exponent of
        // 16..=23 is compared, not truncated.
        if i32::from(header_window_bits) > MAX_WBITS || header_window_bits > effective {
            return Err(ReturnCode::DATA_ERROR);
        }

        Ok(effective)
    }

    /// Resolves the window exponent for a gzip stream.
    ///
    /// A gzip header carries no window size, so there is nothing to negotiate:
    /// when the caller did not ask for an exponent, `inflate` simply uses the
    /// largest one (`inflate.c` L514-L515). Infallible for that reason -- unlike
    /// the zlib case, there is no advertised size that could exceed the request.
    ///
    /// Mirrors `inflate.c` L514-L515.
    #[must_use]
    pub const fn resolve_gzip_window_bits(self) -> u8 {
        if self.window_size_from_header() {
            MAX_WBITS_U8
        } else {
            self.window_bits
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        decode_deflate_window_bits, decode_inflate_window_bits, normalize_deflate_level,
        validate_deflate_flush, validate_deflate_params, validate_deflate_params_change,
        validate_inflate_back_window_bits, validate_mem_level, DeflateConfig, Flush, InflateConfig,
        InflateWrap, Method, Strategy, ValidatedDeflateConfig, ValidatedInflateConfig, Wrap,
        DEF_LEVEL, DEF_MEM_LEVEL, DEF_WBITS, DYN_TREES, GZIP_WRAP_OFFSET, INFLATE_WRAP_GZIP_HEADER,
        INFLATE_WRAP_MASK_LIMIT, INFLATE_WRAP_VERIFY_CHECK, INFLATE_WRAP_ZLIB_HEADER, MAX_MATCH,
        MAX_MEM_LEVEL, MAX_WBITS, MAX_WBITS_U8, MIN_MATCH, MIN_MEM_LEVEL, MIN_WBITS, PRESET_DICT,
        STATIC_TREES, STORED_BLOCK, Z_ASCII, Z_BEST_COMPRESSION, Z_BEST_SPEED, Z_BINARY, Z_BLOCK,
        Z_DEFAULT_COMPRESSION, Z_DEFAULT_STRATEGY, Z_DEFLATED, Z_FILTERED, Z_FINISH, Z_FIXED,
        Z_FULL_FLUSH, Z_HUFFMAN_ONLY, Z_NO_COMPRESSION, Z_NO_FLUSH, Z_PARTIAL_FLUSH, Z_RLE,
        Z_SYNC_FLUSH, Z_TEXT, Z_TREES, Z_UNKNOWN,
    };
    use crate::error::ReturnCode;

    /// Shorthand for the only status any parameter check here produces.
    const BAD_PARAM: ReturnCode = ReturnCode::STREAM_ERROR;

    /// Integers that no parameter of any kind accepts, including both extremes of
    /// the type. `i32::MIN` is the input whose negation is not representable --
    /// the one place a faithful mirror of `windowBits = -windowBits` could overflow
    /// -- and `i32::MAX` is the mirror image for the `- 16` and `>> 4` paths.
    const WILD: [i32; 8] = [
        i32::MIN,
        i32::MIN + 1,
        -1000,
        -100,
        100,
        1000,
        i32::MAX - 1,
        i32::MAX,
    ];

    #[test]
    fn public_constants_carry_the_header_values() {
        // `zlib.h` L194-L197.
        assert_eq!(Z_NO_COMPRESSION, 0);
        assert_eq!(Z_BEST_SPEED, 1);
        assert_eq!(Z_BEST_COMPRESSION, 9);
        assert_eq!(Z_DEFAULT_COMPRESSION, -1);
        // `deflate.c` L419.
        assert_eq!(DEF_LEVEL, 6);

        // `zlib.h` L200-L204.
        assert_eq!(Z_DEFAULT_STRATEGY, 0);
        assert_eq!(Z_FILTERED, 1);
        assert_eq!(Z_HUFFMAN_ONLY, 2);
        assert_eq!(Z_RLE, 3);
        assert_eq!(Z_FIXED, 4);

        // `zlib.h` L207-L210. Z_ASCII is defined *as* Z_TEXT, not as a separate 1.
        assert_eq!(Z_BINARY, 0);
        assert_eq!(Z_TEXT, 1);
        assert_eq!(Z_ASCII, Z_TEXT);
        assert_eq!(Z_UNKNOWN, 2);

        // `zlib.h` L213.
        assert_eq!(Z_DEFLATED, 8);

        // `zlib.h` L172-L178.
        assert_eq!(Z_NO_FLUSH, 0);
        assert_eq!(Z_PARTIAL_FLUSH, 1);
        assert_eq!(Z_SYNC_FLUSH, 2);
        assert_eq!(Z_FULL_FLUSH, 3);
        assert_eq!(Z_FINISH, 4);
        assert_eq!(Z_BLOCK, 5);
        assert_eq!(Z_TREES, 6);

        // `zutil.h` L87-L96.
        assert_eq!(STORED_BLOCK, 0);
        assert_eq!(STATIC_TREES, 1);
        assert_eq!(DYN_TREES, 2);
        assert_eq!(MIN_MATCH, 3);
        assert_eq!(MAX_MATCH, 258);
        assert_eq!(PRESET_DICT, 0x20);
    }

    /// The four bounds that are easiest to conflate, pinned individually.
    /// `MAX_MEM_LEVEL` is 9 (`zconf.h` L272-L279) while `DEF_MEM_LEVEL` is 8
    /// (`zutil.h` L80-L84); `DEF_WBITS` is a separate constant from `MAX_WBITS`
    /// (`zutil.h` L75-L78) even though the shipped build defines one as the other.
    #[test]
    fn window_and_memory_bounds_are_the_shipped_values() {
        assert_eq!(MIN_WBITS, 8);
        assert_eq!(MAX_WBITS, 15);
        assert_eq!(DEF_WBITS, 15);
        assert_eq!(DEF_WBITS, MAX_WBITS);
        assert_eq!(MIN_MEM_LEVEL, 1);
        assert_eq!(MAX_MEM_LEVEL, 9);
        assert_eq!(DEF_MEM_LEVEL, 8);
        assert_ne!(MAX_MEM_LEVEL, DEF_MEM_LEVEL);

        // `zutil.h` L72-L74 refuses to compile outside 9..=15.
        assert!((9..=15).contains(&MAX_WBITS));

        // The private `u8` spelling of MAX_WBITS must agree with the public one.
        assert_eq!(i32::from(MAX_WBITS_U8), MAX_WBITS);

        // The two offsets the encodings are built from (`deflate.c` L431,
        // `inflate.c` L154).
        assert_eq!(GZIP_WRAP_OFFSET, 16);
        assert_eq!(INFLATE_WRAP_MASK_LIMIT, 48);
    }

    #[test]
    fn method_accepts_only_deflated() {
        assert_eq!(Method::from_raw(Z_DEFLATED), Some(Method::Deflated));
        assert_eq!(Method::try_from(Z_DEFLATED), Ok(Method::Deflated));
        assert_eq!(Method::Deflated.as_raw(), Z_DEFLATED);
        assert_eq!(Method::Deflated.c_name(), "Z_DEFLATED");
        assert_eq!(Method::default(), Method::Deflated);
        assert_eq!(Method::ALL.len(), 1);

        // The neighbours of 8, the historical "method 0", and the extremes.
        for raw in [0, 1, 7, 9, 10, -1, -8] {
            assert_eq!(Method::from_raw(raw), None, "method {raw} must be refused");
            assert_eq!(Method::try_from(raw), Err(BAD_PARAM));
        }
        for &raw in &WILD {
            assert_eq!(Method::from_raw(raw), None);
            assert_eq!(Method::try_from(raw), Err(BAD_PARAM));
        }
    }

    /// The five strategies with the exact `int` `zlib.h` L200-L204 gives each and
    /// its reference spelling, in value order.
    const STRATEGIES: [(Strategy, i32, &str); 5] = [
        (Strategy::Default, 0, "Z_DEFAULT_STRATEGY"),
        (Strategy::Filtered, 1, "Z_FILTERED"),
        (Strategy::HuffmanOnly, 2, "Z_HUFFMAN_ONLY"),
        (Strategy::Rle, 3, "Z_RLE"),
        (Strategy::Fixed, 4, "Z_FIXED"),
    ];

    #[test]
    fn strategy_conversions_round_trip() {
        for &(strategy, raw, name) in &STRATEGIES {
            assert_eq!(strategy.as_raw(), raw);
            assert_eq!(strategy.c_name(), name);
            assert_eq!(Strategy::from_raw(raw), Some(strategy));
            assert_eq!(Strategy::try_from(raw), Ok(strategy));
            assert_eq!(Strategy::from_raw(strategy.as_raw()), Some(strategy));
        }

        // `Strategy::ALL` is exhaustive, in value order, and free of duplicates.
        assert_eq!(Strategy::ALL.len(), STRATEGIES.len());
        for (position, (&strategy, &(expected, raw, _))) in
            Strategy::ALL.iter().zip(STRATEGIES.iter()).enumerate()
        {
            assert_eq!(strategy, expected);
            assert_eq!(i32::try_from(position), Ok(raw));
        }
        assert_eq!(Strategy::default(), Strategy::Default);
    }

    #[test]
    fn strategy_rejects_everything_outside_zero_to_four() {
        // The two nearest failures on each side of the accepted range, which is
        // `strategy < 0 || strategy > Z_FIXED` at `deflate.c` L436.
        for raw in [-2, -1, 5, 6, 100] {
            assert_eq!(
                Strategy::from_raw(raw),
                None,
                "strategy {raw} must be refused"
            );
            assert_eq!(Strategy::try_from(raw), Err(BAD_PARAM));
        }
        for &raw in &WILD {
            assert_eq!(Strategy::from_raw(raw), None);
            assert_eq!(Strategy::try_from(raw), Err(BAD_PARAM));
        }
    }

    /// The seven flush modes with their `int`s, their spellings, and whether
    /// `deflate` accepts them (`zlib.h` L172-L178, `deflate.c` L985).
    const FLUSHES: [(Flush, i32, &str, bool); 7] = [
        (Flush::NoFlush, 0, "Z_NO_FLUSH", true),
        (Flush::PartialFlush, 1, "Z_PARTIAL_FLUSH", true),
        (Flush::SyncFlush, 2, "Z_SYNC_FLUSH", true),
        (Flush::FullFlush, 3, "Z_FULL_FLUSH", true),
        (Flush::Finish, 4, "Z_FINISH", true),
        (Flush::Block, 5, "Z_BLOCK", true),
        (Flush::Trees, 6, "Z_TREES", false),
    ];

    #[test]
    fn flush_conversions_round_trip() {
        for &(flush, raw, name, _) in &FLUSHES {
            assert_eq!(flush.as_raw(), raw);
            assert_eq!(flush.c_name(), name);
            assert_eq!(Flush::from_raw(raw), Some(flush));
            assert_eq!(Flush::try_from(raw), Ok(flush));
        }
        assert_eq!(Flush::ALL.len(), FLUSHES.len());
        for (&flush, &(expected, _, _, _)) in Flush::ALL.iter().zip(FLUSHES.iter()) {
            assert_eq!(flush, expected);
        }
        assert_eq!(Flush::default(), Flush::NoFlush);

        for raw in [-2, -1, 7, 8] {
            assert_eq!(Flush::from_raw(raw), None, "flush {raw} names no mode");
            assert_eq!(Flush::try_from(raw), Err(BAD_PARAM));
        }
        for &raw in &WILD {
            assert_eq!(Flush::from_raw(raw), None);
        }
    }

    /// `deflate` rejects `Z_TREES`, which is decompression-only: its guard is
    /// `flush > Z_BLOCK || flush < 0` (`deflate.c` L985). The differential matrix
    /// drives exactly the six modes this leaves.
    #[test]
    fn deflate_accepts_six_of_the_seven_flush_modes() {
        for &(flush, raw, _, valid_for_deflate) in &FLUSHES {
            assert_eq!(flush.is_valid_for_deflate(), valid_for_deflate);
            if valid_for_deflate {
                assert_eq!(validate_deflate_flush(raw), Ok(flush));
            } else {
                assert_eq!(validate_deflate_flush(raw), Err(BAD_PARAM));
            }
        }

        assert_eq!(validate_deflate_flush(Z_TREES), Err(BAD_PARAM));
        assert_eq!(validate_deflate_flush(7), Err(BAD_PARAM));
        assert_eq!(validate_deflate_flush(-1), Err(BAD_PARAM));
        for &raw in &WILD {
            assert_eq!(validate_deflate_flush(raw), Err(BAD_PARAM));
        }

        // Exactly six modes survive, and `Z_BLOCK` is the largest of them.
        let accepted = Flush::ALL
            .iter()
            .filter(|flush| flush.is_valid_for_deflate())
            .count();
        assert_eq!(accepted, 6);
        assert_eq!(Flush::Block.as_raw(), Z_BLOCK);
    }

    #[test]
    fn level_boundaries_match_the_reference() {
        // `Z_DEFAULT_COMPRESSION` resolves to 6 rather than being refused, because
        // the resolution happens before the range check (`deflate.c` L419, L435).
        assert_eq!(normalize_deflate_level(Z_DEFAULT_COMPRESSION), Ok(6));
        assert_eq!(normalize_deflate_level(-1), Ok(6));

        // Every level in range maps to itself.
        for level in 0u8..=9 {
            assert_eq!(normalize_deflate_level(i32::from(level)), Ok(level));
        }
        assert_eq!(normalize_deflate_level(Z_NO_COMPRESSION), Ok(0));
        assert_eq!(normalize_deflate_level(Z_BEST_SPEED), Ok(1));
        assert_eq!(normalize_deflate_level(Z_BEST_COMPRESSION), Ok(9));

        // The nearest failures: -1 is the *only* legal negative, and 9 the ceiling.
        for level in [-2, -3, -10, 10, 11, 100] {
            assert_eq!(
                normalize_deflate_level(level),
                Err(BAD_PARAM),
                "level {level} must be refused"
            );
        }
        for &level in &WILD {
            assert_eq!(normalize_deflate_level(level), Err(BAD_PARAM));
        }
    }

    #[test]
    fn mem_level_boundaries_match_the_reference() {
        for mem_level in 1u8..=9 {
            assert_eq!(validate_mem_level(i32::from(mem_level)), Ok(mem_level));
        }
        assert_eq!(validate_mem_level(MIN_MEM_LEVEL), Ok(1));
        assert_eq!(validate_mem_level(DEF_MEM_LEVEL), Ok(8));
        // Nine is accepted: MAX_MEM_LEVEL is 9, not DEF_MEM_LEVEL's 8.
        assert_eq!(validate_mem_level(MAX_MEM_LEVEL), Ok(9));

        for mem_level in [0, -1, 10, 11] {
            assert_eq!(
                validate_mem_level(mem_level),
                Err(BAD_PARAM),
                "memLevel {mem_level} must be refused"
            );
        }
        for &mem_level in &WILD {
            assert_eq!(validate_mem_level(mem_level), Err(BAD_PARAM));
        }
    }

    #[test]
    fn deflate_wrap_uses_the_zero_one_two_encoding() {
        // The numbering `read_buf` dispatches on (`deflate.c` L228 and L232).
        assert_eq!(Wrap::None.as_deflate_wrap(), 0);
        assert_eq!(Wrap::Zlib.as_deflate_wrap(), 1);
        assert_eq!(Wrap::Gzip.as_deflate_wrap(), 2);

        for &wrap in &Wrap::ALL {
            assert_eq!(Wrap::from_deflate_wrap(wrap.as_deflate_wrap()), Some(wrap));
        }
        assert_eq!(Wrap::ALL.len(), 3);

        // Exactly one container computes each check value, and raw computes none.
        assert!(Wrap::Zlib.computes_adler32());
        assert!(!Wrap::Gzip.computes_adler32());
        assert!(!Wrap::None.computes_adler32());
        assert!(Wrap::Gzip.computes_crc32());
        assert!(!Wrap::Zlib.computes_crc32());
        assert!(!Wrap::None.computes_crc32());

        assert!(Wrap::Zlib.is_wrapped());
        assert!(Wrap::Gzip.is_wrapped());
        assert!(!Wrap::None.is_wrapped());

        for raw in [-1, 3, 4, 5] {
            assert_eq!(Wrap::from_deflate_wrap(raw), None);
        }
        for &raw in &WILD {
            assert_eq!(Wrap::from_deflate_wrap(raw), None);
        }
    }

    #[test]
    fn inflate_wrap_uses_the_bitmask_encoding() {
        // The values `(windowBits >> 4) + 5` produces (`inflate.c` L152).
        assert_eq!(InflateWrap::None.as_inflate_wrap(), 0);
        assert_eq!(InflateWrap::Zlib.as_inflate_wrap(), 5);
        assert_eq!(InflateWrap::Gzip.as_inflate_wrap(), 6);
        assert_eq!(InflateWrap::ZlibOrGzip.as_inflate_wrap(), 7);

        // ... and the bits those values are made of.
        assert_eq!(INFLATE_WRAP_ZLIB_HEADER, 1);
        assert_eq!(INFLATE_WRAP_GZIP_HEADER, 2);
        assert_eq!(INFLATE_WRAP_VERIFY_CHECK, 4);
        for &wrap in &InflateWrap::ALL {
            let raw = wrap.as_inflate_wrap();
            assert_eq!(InflateWrap::from_inflate_wrap(raw), Some(wrap));
            assert_eq!(
                raw & INFLATE_WRAP_ZLIB_HEADER != 0,
                wrap.allows_zlib_header()
            );
            assert_eq!(
                raw & INFLATE_WRAP_GZIP_HEADER != 0,
                wrap.allows_gzip_header()
            );
            assert_eq!(
                raw & INFLATE_WRAP_VERIFY_CHECK != 0,
                wrap.verifies_check_value()
            );
        }
        assert_eq!(InflateWrap::ALL.len(), 4);

        // Automatic detection accepts both headers and pins neither container.
        assert!(InflateWrap::ZlibOrGzip.allows_zlib_header());
        assert!(InflateWrap::ZlibOrGzip.allows_gzip_header());
        assert_eq!(InflateWrap::ZlibOrGzip.determinate_wrap(), None);
        assert_eq!(InflateWrap::None.determinate_wrap(), Some(Wrap::None));
        assert_eq!(InflateWrap::Zlib.determinate_wrap(), Some(Wrap::Zlib));
        assert_eq!(InflateWrap::Gzip.determinate_wrap(), Some(Wrap::Gzip));

        // Raw streams read no header and verify nothing.
        assert!(!InflateWrap::None.allows_zlib_header());
        assert!(!InflateWrap::None.allows_gzip_header());
        assert!(!InflateWrap::None.verifies_check_value());

        // The values `inflateReset2` cannot produce are not accepted back, even
        // though `inflateValidate` can make 1, 2 and 3 appear in a live state.
        for raw in [1, 2, 3, 4, 8, 9, -1] {
            assert_eq!(InflateWrap::from_inflate_wrap(raw), None);
        }
        for &raw in &WILD {
            assert_eq!(InflateWrap::from_inflate_wrap(raw), None);
        }
    }

    #[test]
    fn deflate_window_bits_accepts_the_three_containers() {
        // A zlib request is the exponent itself, 9..=15.
        for exponent in 9u8..=15 {
            assert_eq!(
                decode_deflate_window_bits(i32::from(exponent)),
                Ok((Wrap::Zlib, exponent))
            );
        }
        // A raw request is the negated exponent, and 8 is excluded (see below).
        for exponent in 9u8..=15 {
            assert_eq!(
                decode_deflate_window_bits(-i32::from(exponent)),
                Ok((Wrap::None, exponent))
            );
        }
        // A gzip request is the exponent plus 16, so 25..=31.
        for exponent in 9u8..=15 {
            assert_eq!(
                decode_deflate_window_bits(i32::from(exponent) + GZIP_WRAP_OFFSET),
                Ok((Wrap::Gzip, exponent))
            );
        }
        // The two ends of the gzip family, spelled out as literals.
        assert_eq!(decode_deflate_window_bits(25), Ok((Wrap::Gzip, 9)));
        assert_eq!(decode_deflate_window_bits(31), Ok((Wrap::Gzip, 15)));
        assert_eq!(decode_deflate_window_bits(MAX_WBITS), Ok((Wrap::Zlib, 15)));
        assert_eq!(decode_deflate_window_bits(-MAX_WBITS), Ok((Wrap::None, 15)));
    }

    /// `deflate.c` L439: a request for a 256-byte window becomes a 512-byte one,
    /// "until 256-byte window bug fixed".
    #[test]
    fn deflate_promotes_eight_to_nine_for_zlib_only() {
        assert_eq!(decode_deflate_window_bits(MIN_WBITS), Ok((Wrap::Zlib, 9)));
        assert_eq!(decode_deflate_window_bits(8), Ok((Wrap::Zlib, 9)));

        // ... and refuses it outright for raw and gzip, because only the zlib
        // header can transmit the window size (`deflate.c` L436).
        assert_eq!(decode_deflate_window_bits(-8), Err(BAD_PARAM));
        assert_eq!(
            decode_deflate_window_bits(8 + GZIP_WRAP_OFFSET),
            Err(BAD_PARAM)
        );
        assert_eq!(decode_deflate_window_bits(24), Err(BAD_PARAM));

        // No accepted request ever yields an exponent of 8.
        for window_bits in -50..=50 {
            if let Ok((_, exponent)) = decode_deflate_window_bits(window_bits) {
                assert!(
                    (9..=15).contains(&exponent),
                    "windowBits {window_bits} yielded exponent {exponent}"
                );
            }
        }
    }

    #[test]
    fn deflate_window_bits_rejects_everything_else() {
        // Zero is inflate's "size from the header" request and means nothing here;
        // 1..=7 and 16..=24 decode to windows below the minimum; 32 and up decode
        // above the maximum, so inflate's `+32` mode has no counterpart.
        let mut rejected = alloc_free_list();
        rejected.push_range(0..=7);
        rejected.push_range(16..=24);
        rejected.push_range(32..=48);
        rejected.push_range(-8..=-1);
        rejected.push_range(-64..=-16);
        for window_bits in rejected.iter() {
            assert_eq!(
                decode_deflate_window_bits(window_bits),
                Err(BAD_PARAM),
                "deflate windowBits {window_bits} must be refused"
            );
        }
        for &window_bits in &WILD {
            assert_eq!(decode_deflate_window_bits(window_bits), Err(BAD_PARAM));
        }
    }

    #[test]
    fn inflate_window_bits_accepts_four_wrap_requests() {
        // zlib: 8..=15, and note that 8 is *not* promoted.
        for exponent in 8u8..=15 {
            assert_eq!(
                decode_inflate_window_bits(i32::from(exponent)),
                Ok((InflateWrap::Zlib, exponent))
            );
        }
        // raw: -15..=-8, and -8 is accepted where deflate refuses it.
        for exponent in 8u8..=15 {
            assert_eq!(
                decode_inflate_window_bits(-i32::from(exponent)),
                Ok((InflateWrap::None, exponent))
            );
        }
        // gzip only: +16, so 24..=31.
        for exponent in 8u8..=15 {
            assert_eq!(
                decode_inflate_window_bits(i32::from(exponent) + 16),
                Ok((InflateWrap::Gzip, exponent))
            );
        }
        // automatic detection: +32, so 40..=47.
        for exponent in 8u8..=15 {
            assert_eq!(
                decode_inflate_window_bits(i32::from(exponent) + 32),
                Ok((InflateWrap::ZlibOrGzip, exponent))
            );
        }
        assert_eq!(
            decode_inflate_window_bits(DEF_WBITS),
            Ok((InflateWrap::Zlib, 15))
        );
        assert_eq!(
            decode_inflate_window_bits(47),
            Ok((InflateWrap::ZlibOrGzip, 15))
        );
    }

    /// Zero is exempt from the range check, and that exemption *is* the "take the
    /// window size from the stream header" feature (`inflate.c` L160).
    #[test]
    fn inflate_window_bits_zero_defers_to_the_header() {
        assert_eq!(decode_inflate_window_bits(0), Ok((InflateWrap::Zlib, 0)));
        assert_eq!(decode_inflate_window_bits(16), Ok((InflateWrap::Gzip, 0)));
        assert_eq!(
            decode_inflate_window_bits(32),
            Ok((InflateWrap::ZlibOrGzip, 0))
        );
    }

    #[test]
    fn inflate_window_bits_rejects_the_gaps_and_everything_from_48_up() {
        let mut rejected = alloc_free_list();
        // Below the minimum window in each of the three non-negative families.
        rejected.push_range(1..=7);
        rejected.push_range(17..=23);
        rejected.push_range(33..=39);
        // 48 and above keep their full value and so fail the range check, which
        // is what stops `wrap` from ever exceeding 7 (`inflate.c` L154-L155).
        rejected.push_range(48..=80);
        rejected.push_range(-7..=-1);
        rejected.push_range(-64..=-16);
        for window_bits in rejected.iter() {
            assert_eq!(
                decode_inflate_window_bits(window_bits),
                Err(BAD_PARAM),
                "inflate windowBits {window_bits} must be refused"
            );
        }
        for &window_bits in &WILD {
            assert_eq!(decode_inflate_window_bits(window_bits), Err(BAD_PARAM));
        }
    }

    /// Independent check of the wrap arithmetic: for every accepted non-negative
    /// request, the stored `wrap` must equal `(windowBits >> 4) + 5` exactly, as
    /// `inflate.c` L152 computes it.
    #[test]
    fn inflate_wrap_follows_the_shift_and_bias() {
        for window_bits in 0..INFLATE_WRAP_MASK_LIMIT {
            if let Ok((wrap, _)) = decode_inflate_window_bits(window_bits) {
                assert_eq!(
                    wrap.as_inflate_wrap(),
                    (window_bits >> 4) + 5,
                    "wrap for windowBits {window_bits}"
                );
            }
        }
        // And a raw request stores zero (`inflate.c` L148).
        for exponent in 8u8..=15 {
            assert_eq!(
                decode_inflate_window_bits(-i32::from(exponent))
                    .map(|(wrap, _)| wrap.as_inflate_wrap()),
                Ok(0)
            );
        }
    }

    /// The four places the two directions genuinely disagree. Sharing one decoder
    /// between them would break at least one of these.
    #[test]
    fn the_two_directions_disagree_exactly_where_the_reference_does() {
        // 1. deflate promotes 8 to 9; inflate leaves 8 alone.
        assert_eq!(decode_deflate_window_bits(8), Ok((Wrap::Zlib, 9)));
        assert_eq!(decode_inflate_window_bits(8), Ok((InflateWrap::Zlib, 8)));

        // 2. deflate refuses a 256-byte raw window; inflate accepts one.
        assert_eq!(decode_deflate_window_bits(-8), Err(BAD_PARAM));
        assert_eq!(decode_inflate_window_bits(-8), Ok((InflateWrap::None, 8)));

        // 3. only inflate understands "size from the header".
        assert_eq!(decode_deflate_window_bits(0), Err(BAD_PARAM));
        assert_eq!(decode_inflate_window_bits(0), Ok((InflateWrap::Zlib, 0)));

        // 4. only inflate understands automatic header detection.
        assert_eq!(decode_deflate_window_bits(40), Err(BAD_PARAM));
        assert_eq!(
            decode_inflate_window_bits(40),
            Ok((InflateWrap::ZlibOrGzip, 8))
        );

        // And the one place they agree that is easy to get wrong: 24 is a
        // 256-byte gzip window, refused by deflate and accepted by inflate.
        assert_eq!(decode_deflate_window_bits(24), Err(BAD_PARAM));
        assert_eq!(decode_inflate_window_bits(24), Ok((InflateWrap::Gzip, 8)));
    }

    /// `inflateBackInit_` takes `8..=15` and nothing else: no container encoding,
    /// no zero, no negatives (`infback.c` L33-L35).
    #[test]
    fn inflate_back_window_bits_is_the_narrowest_rule() {
        for exponent in 8u8..=15 {
            assert_eq!(
                validate_inflate_back_window_bits(i32::from(exponent)),
                Ok(exponent)
            );
        }
        assert_eq!(validate_inflate_back_window_bits(MIN_WBITS), Ok(8));
        assert_eq!(validate_inflate_back_window_bits(MAX_WBITS), Ok(15));

        // 7 and 16 are the nearest failures; 0, the negatives and the `+16`/`+32`
        // encodings are all refused, unlike for `inflateInit2_`.
        for window_bits in [7, 16, 0, 24, 32, 40, 47, 48, -8, -15, -1] {
            assert_eq!(
                validate_inflate_back_window_bits(window_bits),
                Err(BAD_PARAM),
                "inflateBack windowBits {window_bits} must be refused"
            );
        }
        for &window_bits in &WILD {
            assert_eq!(
                validate_inflate_back_window_bits(window_bits),
                Err(BAD_PARAM)
            );
        }
    }

    #[test]
    fn deflate_defaults_are_the_deflate_init_call() {
        // `deflate.c` L379-L383.
        let config = DeflateConfig::default();
        assert_eq!(config.level, Z_DEFAULT_COMPRESSION);
        assert_eq!(config.method, Method::Deflated);
        assert_eq!(config.window_bits, MAX_WBITS);
        assert_eq!(config.mem_level, DEF_MEM_LEVEL);
        assert_eq!(config.strategy, Strategy::Default);
        assert_eq!(config, DeflateConfig::new(Z_DEFAULT_COMPRESSION));

        // `DeflateConfig::new` changes the level and nothing else.
        for level in [Z_NO_COMPRESSION, Z_BEST_SPEED, 6, Z_BEST_COMPRESSION] {
            let config = DeflateConfig::new(level);
            assert_eq!(config.level, level);
            assert_eq!(config.window_bits, MAX_WBITS);
            assert_eq!(config.mem_level, DEF_MEM_LEVEL);
            assert_eq!(config.strategy, Strategy::Default);
        }

        // Validating the default resolves the level and the container.
        assert_eq!(
            DeflateConfig::default().validate(),
            Ok(ValidatedDeflateConfig {
                level: 6,
                method: Method::Deflated,
                wrap: Wrap::Zlib,
                window_bits: 15,
                mem_level: 8,
                strategy: Strategy::Default,
            })
        );
    }

    #[test]
    fn inflate_defaults_are_the_inflate_init_call() {
        // `inflate.c` L214-L217.
        let config = InflateConfig::default();
        assert_eq!(config.window_bits, DEF_WBITS);
        assert_eq!(config, InflateConfig::new(DEF_WBITS));
        assert_eq!(
            config.validate(),
            Ok(ValidatedInflateConfig {
                wrap: InflateWrap::Zlib,
                window_bits: 15,
            })
        );

        // A gzip-only decoder that takes its window size from the header.
        assert_eq!(
            InflateConfig::new(16).validate(),
            Ok(ValidatedInflateConfig {
                wrap: InflateWrap::Gzip,
                window_bits: 0,
            })
        );
        assert_eq!(InflateConfig::new(7).validate(), Err(BAD_PARAM));
    }

    #[test]
    fn deflate_config_from_raw_types_the_method_and_strategy() {
        assert_eq!(
            DeflateConfig::from_raw(6, Z_DEFLATED, 15, 8, Z_RLE),
            Ok(DeflateConfig {
                level: 6,
                method: Method::Deflated,
                window_bits: 15,
                mem_level: 8,
                strategy: Strategy::Rle,
            })
        );

        // A bad method or strategy is refused here; the three integers are not
        // checked until `validate`, and the code is the same either way.
        assert_eq!(
            DeflateConfig::from_raw(6, 0, 15, 8, Z_DEFAULT_STRATEGY),
            Err(BAD_PARAM)
        );
        assert_eq!(
            DeflateConfig::from_raw(6, Z_DEFLATED, 15, 8, 5),
            Err(BAD_PARAM)
        );
        assert_eq!(
            DeflateConfig::from_raw(1000, Z_DEFLATED, 1000, 1000, Z_FIXED)
                .map(DeflateConfig::validate),
            Ok(Err(BAD_PARAM))
        );
    }

    /// A miniature of the differential matrix: every level, every memory level,
    /// every strategy and every container form must be accepted together, and the
    /// resolved values must be the ones the reference would store.
    #[test]
    fn the_whole_parameter_matrix_is_accepted() {
        let mut combinations = 0_u32;
        for level in -1i32..=9 {
            for mem_level in MIN_MEM_LEVEL..=MAX_MEM_LEVEL {
                for &strategy in &Strategy::ALL {
                    for exponent in 9u8..=15 {
                        let bits = i32::from(exponent);
                        let cases = [
                            (bits, Wrap::Zlib, exponent),
                            (-bits, Wrap::None, exponent),
                            (bits + GZIP_WRAP_OFFSET, Wrap::Gzip, exponent),
                        ];
                        for &(window_bits, wrap, expected_exponent) in &cases {
                            let expected_level = if level == Z_DEFAULT_COMPRESSION {
                                DEF_LEVEL
                            } else {
                                level
                            };
                            assert_eq!(
                                validate_deflate_params(
                                    level,
                                    Z_DEFLATED,
                                    window_bits,
                                    mem_level,
                                    strategy.as_raw()
                                )
                                .map(|config| (
                                    i32::from(config.level),
                                    config.wrap,
                                    config.window_bits,
                                    i32::from(config.mem_level),
                                    config.strategy
                                )),
                                Ok((expected_level, wrap, expected_exponent, mem_level, strategy)),
                                "level {level}, memLevel {mem_level}, windowBits {window_bits}"
                            );
                            combinations += 1;
                        }
                    }
                }
            }
        }
        // 11 levels x 9 memory levels x 5 strategies x 7 exponents x 3 containers.
        assert_eq!(combinations, 11 * 9 * 5 * 7 * 3);
    }

    #[test]
    fn one_bad_parameter_fails_the_whole_configuration() {
        // Each row changes exactly one parameter of an otherwise valid call.
        let cases = [
            (-2, Z_DEFLATED, 15, 8, Z_DEFAULT_STRATEGY),
            (10, Z_DEFLATED, 15, 8, Z_DEFAULT_STRATEGY),
            (6, 0, 15, 8, Z_DEFAULT_STRATEGY),
            (6, 9, 15, 8, Z_DEFAULT_STRATEGY),
            (6, Z_DEFLATED, 0, 8, Z_DEFAULT_STRATEGY),
            (6, Z_DEFLATED, 7, 8, Z_DEFAULT_STRATEGY),
            (6, Z_DEFLATED, 16, 8, Z_DEFAULT_STRATEGY),
            (6, Z_DEFLATED, 32, 8, Z_DEFAULT_STRATEGY),
            (6, Z_DEFLATED, 48, 8, Z_DEFAULT_STRATEGY),
            (6, Z_DEFLATED, -8, 8, Z_DEFAULT_STRATEGY),
            (6, Z_DEFLATED, 15, 0, Z_DEFAULT_STRATEGY),
            (6, Z_DEFLATED, 15, 10, Z_DEFAULT_STRATEGY),
            (6, Z_DEFLATED, 15, 8, -1),
            (6, Z_DEFLATED, 15, 8, 5),
        ];
        for &(level, method, window_bits, mem_level, strategy) in &cases {
            assert_eq!(
                validate_deflate_params(level, method, window_bits, mem_level, strategy),
                Err(BAD_PARAM),
                "({level}, {method}, {window_bits}, {mem_level}, {strategy})"
            );
        }

        // The same call with every parameter valid is accepted, so each row above
        // fails for the reason it names.
        assert!(validate_deflate_params(6, Z_DEFLATED, 15, 8, Z_DEFAULT_STRATEGY).is_ok());
    }

    /// `deflateParams` applies `deflateInit2_`'s level and strategy rules, default
    /// resolution included (`deflate.c` L784-L788).
    #[test]
    fn params_change_shares_the_init_bounds() {
        assert_eq!(
            validate_deflate_params_change(Z_DEFAULT_COMPRESSION, Z_DEFAULT_STRATEGY),
            Ok((6, Strategy::Default))
        );
        assert_eq!(
            validate_deflate_params_change(9, Z_FIXED),
            Ok((9, Strategy::Fixed))
        );
        assert_eq!(
            validate_deflate_params_change(0, Z_HUFFMAN_ONLY),
            Ok((0, Strategy::HuffmanOnly))
        );
        for &(level, strategy) in &[
            (-2, 0),
            (10, 0),
            (0, -1),
            (0, 5),
            (i32::MIN, 0),
            (0, i32::MAX),
        ] {
            assert_eq!(
                validate_deflate_params_change(level, strategy),
                Err(BAD_PARAM),
                "deflateParams({level}, {strategy})"
            );
        }
    }

    #[test]
    fn zlib_header_window_size_is_adopted_or_refused() {
        let from_header = ValidatedInflateConfig {
            wrap: InflateWrap::Zlib,
            window_bits: 0,
        };
        assert!(from_header.window_size_from_header());

        // No request: the header's exponent is adopted, for every legal value.
        for header in 8u8..=15 {
            assert_eq!(
                from_header.resolve_zlib_header_window_bits(header),
                Ok(header)
            );
        }
        // ... but a header claiming more than 15 is refused even then, because the
        // `len > 15` test comes after the adoption (`inflate.c` L542).
        for header in 16u8..=23 {
            assert_eq!(
                from_header.resolve_zlib_header_window_bits(header),
                Err(ReturnCode::DATA_ERROR),
                "header exponent {header} must be refused"
            );
        }

        // A request of 15 accepts every legal header and keeps its own exponent,
        // because inflate may use a window larger than the stream needs.
        let widest = ValidatedInflateConfig {
            wrap: InflateWrap::Zlib,
            window_bits: 15,
        };
        assert!(!widest.window_size_from_header());
        for header in 8u8..=15 {
            assert_eq!(widest.resolve_zlib_header_window_bits(header), Ok(15));
        }

        // A narrower request refuses a stream that needs more: this is the
        // documented `Z_DATA_ERROR` of `zlib.h` L869-L873, and *not* a
        // `Z_STREAM_ERROR`, because the parameters were never the problem.
        let narrow = ValidatedInflateConfig {
            wrap: InflateWrap::Zlib,
            window_bits: 10,
        };
        assert_eq!(narrow.resolve_zlib_header_window_bits(10), Ok(10));
        assert_eq!(narrow.resolve_zlib_header_window_bits(9), Ok(10));
        for header in 11u8..=15 {
            assert_eq!(
                narrow.resolve_zlib_header_window_bits(header),
                Err(ReturnCode::DATA_ERROR)
            );
        }

        // The interoperability trap `zlib.h` L562-L568 warns about: a stream
        // deflate wrote after promoting 8 to 9 records 9, so inflating it with 8
        // fails.
        let promoted_mismatch = ValidatedInflateConfig {
            wrap: InflateWrap::Zlib,
            window_bits: 8,
        };
        assert_eq!(
            promoted_mismatch.resolve_zlib_header_window_bits(9),
            Err(ReturnCode::DATA_ERROR)
        );
        assert_eq!(promoted_mismatch.resolve_zlib_header_window_bits(8), Ok(8));
    }

    #[test]
    fn gzip_window_size_falls_back_to_the_maximum() {
        // A gzip header carries no window size, so an unspecified exponent simply
        // becomes the largest one (`inflate.c` L514-L515).
        let from_header = ValidatedInflateConfig {
            wrap: InflateWrap::Gzip,
            window_bits: 0,
        };
        assert_eq!(from_header.resolve_gzip_window_bits(), MAX_WBITS_U8);
        assert_eq!(from_header.resolve_gzip_window_bits(), 15);

        for exponent in 8u8..=15 {
            let config = ValidatedInflateConfig {
                wrap: InflateWrap::Gzip,
                window_bits: exponent,
            };
            assert_eq!(config.resolve_gzip_window_bits(), exponent);
        }
    }

    /// Every fallible entry point must refuse the extremes of `i32` rather than
    /// overflowing. `i32::MIN` is the interesting one: the reference's
    /// `windowBits = -windowBits` would be undefined behaviour for it in C and a
    /// panic in debug Rust, and it is only unreachable because the sub-range check
    /// runs first. Reaching this assertion at all is the proof -- a panic would
    /// fail the test.
    #[test]
    fn the_extremes_of_the_integer_type_are_refused_without_panicking() {
        for &wild in &WILD {
            assert_eq!(decode_deflate_window_bits(wild), Err(BAD_PARAM));
            assert_eq!(decode_inflate_window_bits(wild), Err(BAD_PARAM));
            assert_eq!(validate_inflate_back_window_bits(wild), Err(BAD_PARAM));
            assert_eq!(normalize_deflate_level(wild), Err(BAD_PARAM));
            assert_eq!(validate_mem_level(wild), Err(BAD_PARAM));
            assert_eq!(validate_deflate_flush(wild), Err(BAD_PARAM));
            assert_eq!(Method::try_from(wild), Err(BAD_PARAM));
            assert_eq!(Strategy::try_from(wild), Err(BAD_PARAM));
            assert_eq!(Flush::try_from(wild), Err(BAD_PARAM));
            assert_eq!(
                validate_deflate_params(wild, wild, wild, wild, wild),
                Err(BAD_PARAM)
            );
            assert_eq!(validate_deflate_params_change(wild, wild), Err(BAD_PARAM));
            assert_eq!(InflateConfig::new(wild).validate(), Err(BAD_PARAM));
            assert_eq!(DeflateConfig::new(wild).validate(), Err(BAD_PARAM));
        }

        // The exact boundary of the negative sub-range check, on both sides.
        assert_eq!(decode_deflate_window_bits(-15), Ok((Wrap::None, 15)));
        assert_eq!(decode_deflate_window_bits(-16), Err(BAD_PARAM));
        assert_eq!(decode_inflate_window_bits(-15), Ok((InflateWrap::None, 15)));
        assert_eq!(decode_inflate_window_bits(-16), Err(BAD_PARAM));
    }

    /// A fixed-capacity list of `i32`s, so the rejection tables can be assembled
    /// from ranges without an allocator: the crate is `no_std` and does not link
    /// `alloc`.
    struct IntList {
        values: [i32; 256],
        len: usize,
    }

    impl IntList {
        /// Appends every integer in `range`, ignoring anything past capacity --
        /// which the tests never reach, and which a `get_mut` miss would silently
        /// drop rather than panic on.
        fn push_range(&mut self, range: core::ops::RangeInclusive<i32>) {
            for value in range {
                if let Some(slot) = self.values.get_mut(self.len) {
                    *slot = value;
                    self.len += 1;
                }
            }
        }

        /// The integers appended so far.
        fn iter(&self) -> impl Iterator<Item = i32> + '_ {
            self.values.iter().copied().take(self.len)
        }
    }

    /// A new empty list. Named for what it guarantees: no heap involvement.
    fn alloc_free_list() -> IntList {
        IntList {
            values: [0; 256],
            len: 0,
        }
    }
}
