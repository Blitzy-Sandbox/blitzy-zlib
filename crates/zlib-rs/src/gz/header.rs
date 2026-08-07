//! The RFC 1952 field vocabulary of the `gzFile` layer: named constants, the documented member
//! layout, and the six slice helpers this layer owns.
//!
//! `gzlib.c`, `gzread.c`, `gzwrite.c` and `gzclose.c` -- the four translation units the `gzFile`
//! layer is built from -- contain between them exactly **one** reference to a gzip header byte: the
//! four-byte magic test at `gzread.c` L151-L153. This module implements that test plus the
//! vocabulary that makes the rest of the layer legible. Everything else about the container is the
//! business of the compression engines, and the next section says so precisely, because the most
//! important property of this file is what it does *not* contain.
//!
//! # What this module deliberately does not do
//!
//! There is no header state machine here, no header emitter and no trailer verifier. Both
//! directions of the `gzFile` layer hand the container to the engine by asking for a window size
//! with 16 added to it, which is zlib's request for the gzip wrapper rather than the zlib one:
//!
//! | Direction | C call site | Consequence |
//! |---|---|---|
//! | writing | `gz_init` calls `deflateInit2(strm, level, Z_DEFLATED, MAX_WBITS + 16, DEF_MEM_LEVEL, strategy)` -- `gzwrite.c` L36-L37 | `crates/zlib-rs/src/deflate/mod.rs` emits the fixed header, the optional fields and the trailer |
//! | reading | `gz_look` calls `inflateInit2(&state->strm, 15 + 16)` -- `gzread.c` L115 | `crates/zlib-rs/src/inflate/header.rs` parses the header and checks the trailer |
//!
//! The C sources make that division of labour checkable rather than merely intended: grepping all
//! four `gz*.c` files for `31`, `139`, `0x1f` or `0x8b` yields a single hit, `gzread.c` L152. Any
//! byte of a gzip member that the file layer appears to handle is in fact handled by `deflate` or
//! `inflate`, which is why the only predicate defined below is `looks_like_gzip` and why nothing
//! below keeps state between calls.
//!
//! What is left for this module is therefore small and entirely declarative:
//!
//! 1. the four-byte magic predicate `gz_look` uses to choose between decompressing and copying;
//! 2. named constants and a documented layout, so that `read.rs`, `write.rs` and `printf.rs` -- and
//!    whoever maintains them next -- can reason about member boundaries without re-deriving the
//!    format from the RFC;
//! 3. length-checked little-endian codecs for the 16- and 32-bit fields of a member, which are
//!    `XLEN`, a `FEXTRA` subfield `LEN`, `CRC16`, `MTIME`, `CRC32` and `ISIZE`;
//! 4. `header_crc16`, the one derived value the format defines over header bytes rather than over
//!    payload.
//!
//! # Member layout
//!
//! A member is a fixed 10-byte header -- `ID1`, `ID2`, `CM`, `FLG`, `MTIME`, `XFL`, `OS` -- then
//! the optional `FEXTRA`, `FNAME`, `FCOMMENT` and `FHCRC` fields in that order, each present only
//! when its `FLG` bit is set, then the compressed blocks, then an 8-byte trailer of `CRC32` and
//! `ISIZE` (`doc/rfc1952.txt` L242-L277, the authority for the diagram and the field widths). Every
//! multi-byte integer is little-endian.
//!
//! A `.gz` file is a sequence of such members, and both `gzread.c` and the reference `gzip`
//! implementation read them one after another; `gz_look` is called again for each one, which is why
//! the magic predicate has to be cheap and stateless.
//!
//! # The extra field
//!
//! When `FLG.FEXTRA` is set, the `XLEN` bytes that follow the fixed header are a series of
//! subfields, each of the form `SI1 SI2 LEN` followed by `LEN` bytes of data
//! (`doc/rfc1952.txt` L431-L461). `SI1` and `SI2` are a two-byte subfield identifier, `LEN` is
//! little-endian, and `LEN` counts only the data -- "LEN gives the length of the subfield data,
//! excluding the 4 initial bytes" (RFC L459-L460). `XLEN` counts the subfields in total and does
//! not include itself. Hence the two widths below, `XLEN_LEN` and `SUBFIELD_HEADER_LEN`.
//!
//! # The two optional strings
//!
//! `FNAME` and `FCOMMENT` are ISO 8859-1 (LATIN-1) byte strings terminated by a zero byte
//! (`doc/rfc1952.txt` L343-L360). Two consequences are worth stating because they shape every
//! signature in the `gz` layer that touches them:
//!
//! * They are **bytes, not Rust strings.** ISO 8859-1 is not a subset of UTF-8 above `0x7f`, so a
//!   valid file name may not be valid UTF-8; nothing here or in the layer above may assume
//!   otherwise. `zlib.h` L126-L129 types both as `Bytef *` for the same reason.
//! * Because the terminator is a zero byte, neither string can contain an interior zero, and the
//!   length of each is therefore implied by its content rather than carried in the header.
//!
//! # Compliance
//!
//! `doc/rfc1952.txt` L462-L480 splits the obligation between compressor and decompressor, and this
//! implementation is on both sides of it. The compressor's five mandatory fields -- `ID1`, `ID2`,
//! `CM`, `CRC32` and `ISIZE` -- are emitted by `deflate` (`deflate.c` L1069-L1071 and L1269-L1276),
//! with the permitted defaults at L1073-L1081. The decompressor's mandatory checks are performed by
//! `inflate`: the magic word at `inflate.c` L513, the compression method at L562, and the reserved
//! bits at L563. The last of those is not optional — a set reserved bit may signal a new field that
//! would make everything after it be read wrongly (RFC L477-L480) — which is why the predicate below
//! tests it too rather than deferring it.
//!
//! # How this module reports failure
//!
//! With [`Option`], not with `crate::error::ReturnCode`. Every failure a function here can have is a
//! caller passing fewer bytes than the field it asked about occupies, and a length shortfall does
//! not by itself determine a zlib status code: the same four missing bytes mean "wait for more
//! input" to `gz_look` (`gzread.c` L143-L146) and `Z_BUF_ERROR` to a caller that has reached end of
//! file. Returning `None` leaves that decision where the information is. Nothing here panics,
//! indexes without a bound check, allocates, or borrows for longer than the call.
//!
//! # Environment
//!
//! `core` only: this module needs neither `alloc` nor `std`, even though the crate root gates the
//! whole `gz` tree on the `std` feature because its siblings perform file I/O. That is why no
//! per-item `cfg` attribute appears below.

// `dead_code` is allowed for this module, and for this module alone, because a format vocabulary is
// complete when it covers the format rather than when every entry happens to have a caller.
// `FLG_FCOMMENT` and `XFL_FASTEST` name bytes this library reads and writes whether or not any Rust
// function mentions them today, and deleting the entries that are momentarily unquoted would leave
// the next maintainer to re-derive them from `doc/rfc1952.txt` -- which is the cost this module
// exists to remove. There is no other `allow` anywhere in this file, and this one is scoped to the
// module rather than to the crate, so dead code elsewhere in `zlib-rs` still fails the build.
//
// Two measured facts made the attribute necessary rather than merely tidy, and both are worth
// recording because neither is obvious:
//
//   * Everything here is `pub(crate)`, mirroring the `local` and `ZLIB_INTERNAL` linkage of the C
//     sources, so nothing is reachable from the crate root and rustc reports any entry whose callers
//     are not yet written. `crates/zlib-rs/src/gz/read.rs` is the consumer of `looks_like_gzip`.
//   * The `const _: () = assert!(...)` invariants below do NOT count as uses of a constant on the
//     declared MSRV. Building this module with rustc 1.80.0 reports all nineteen constants as never
//     used; building it with rustc 1.97.1 reports none of them. So the attribute is what keeps the
//     MSRV toolchain's build warning-free even once every sibling module has landed, and removing it
//     would reintroduce nineteen warnings on the very toolchain `rust-version` promises to support.
#![allow(dead_code)]

use crate::crc32::crc32;

/// Width in bytes of a little-endian 16-bit field of a gzip member.
///
/// Three fields have this width: `XLEN` (`doc/rfc1952.txt` L248-L250 and L415-L417), the `LEN` of a
/// `FEXTRA` subfield (L437-L439) and the optional `CRC16` (L266-L268). Used by [`u16_le`] and
/// [`write_u16_le`], which read and write exactly this many bytes.
const U16_LEN: usize = 2;

/// Width in bytes of a little-endian 32-bit field of a gzip member.
///
/// Three fields have this width: `MTIME` in the fixed header (`doc/rfc1952.txt` L243 and L364-L372)
/// and `CRC32` and `ISIZE` in the trailer (L274-L277). Used by [`u32_le`] and [`write_u32_le`],
/// which read and write exactly this many bytes.
const U32_LEN: usize = 4;

/// First identification byte of a gzip member: `31`, that is `0x1f`.
///
/// "These have the fixed values ID1 = 31 (0x1f, \\037), ID2 = 139 (0x8b, \\213), to identify the
/// file as being in gzip format" -- `doc/rfc1952.txt` L291-L293. `deflate.c` L1069 writes it and
/// `gzread.c` L152 tests for it.
pub(crate) const ID1: u8 = 31;

/// Second identification byte of a gzip member: `139`, that is `0x8b`.
///
/// See [`ID1`]; `doc/rfc1952.txt` L291-L293. `deflate.c` L1070 writes it and `gzread.c` L152 tests
/// for it.
pub(crate) const ID2: u8 = 139;

/// The two identification bytes as the one 16-bit little-endian word `inflate` matches against.
///
/// `inflate.c` L513 reads sixteen bits into `hold` and compares `hold == 0x8b1f`, which is
/// [`ID1`] followed by [`ID2`] on the wire. The constant exists so that the file layer's
/// byte-at-a-time test and the engine's word-at-a-time test are visibly the same test; the
/// invariant below pins that equality at compile time.
pub(crate) const MAGIC_LE: u16 = 0x8b1f;

/// The `CM` value that denotes the DEFLATE compression method: `8`.
///
/// "CM = 0-7 are reserved. CM = 8 denotes the \"deflate\" compression method"
/// -- `doc/rfc1952.txt` L294-L299. This is the only value this library writes (`deflate.c` L1071)
/// or accepts (`inflate.c` L562, which rejects anything else with "unknown compression method").
pub(crate) const CM_DEFLATE: u8 = 8;

/// `FLG.FTEXT`, bit 0: the payload is probably ASCII text.
///
/// `doc/rfc1952.txt` L303 and L312-L323. Purely advisory -- the RFC deliberately does not specify
/// how a compressor decides, "since a compressor always has the option of leaving it cleared and a
/// decompressor always has the option of ignoring it". `deflate.c` L1092 sets it from
/// `gz_header.text`; `inflate.c` L569 reports it back as `(hold >> 8) & 1`.
pub(crate) const FLG_FTEXT: u8 = 0x01;

/// `FLG.FHCRC`, bit 1: a `CRC16` over the header bytes follows the optional fields.
///
/// `doc/rfc1952.txt` L304 and L325-L331. See [`header_crc16`] for the value itself.
/// `deflate.c` L1093 sets the bit and L1110-L1112 accumulates the check; `inflate.c` L677-L682
/// verifies it and fails with "header crc mismatch".
pub(crate) const FLG_FHCRC: u8 = 0x02;

/// `FLG.FEXTRA`, bit 2: an extra field, `XLEN` bytes long, follows the fixed header.
///
/// `doc/rfc1952.txt` L305 and L333-L334, with the subfield layout at L431-L461. `deflate.c` L1094
/// sets the bit and L1107-L1108 writes `XLEN`; `inflate.c` L595-L604 reads it.
pub(crate) const FLG_FEXTRA: u8 = 0x04;

/// `FLG.FNAME`, bit 3: a zero-terminated original file name follows.
///
/// `doc/rfc1952.txt` L306 and L343-L354. ISO 8859-1, so bytes rather than text. `deflate.c` L1095
/// sets the bit; `inflate.c` L633-L649 reads the name, clamped to `gz_header.name_max`.
pub(crate) const FLG_FNAME: u8 = 0x08;

/// `FLG.FCOMMENT`, bit 4: a zero-terminated human-readable comment follows.
///
/// `doc/rfc1952.txt` L307 and L356-L360. ISO 8859-1, so bytes rather than text, and "not
/// interpreted; it is only intended for human consumption". `deflate.c` L1096 sets the bit;
/// `inflate.c` L655-L674 reads the comment, clamped to `gz_header.comm_max`.
pub(crate) const FLG_FCOMMENT: u8 = 0x10;

/// The five defined `FLG` bits, or-ed together: `0x1f`.
///
/// Equivalently, every bit of the flag byte that a compliant member may set. Its complement is
/// [`FLG_RESERVED`], and the two tile the byte exactly, which the invariant below checks.
pub(crate) const FLG_KNOWN: u8 = FLG_FTEXT | FLG_FHCRC | FLG_FEXTRA | FLG_FNAME | FLG_FCOMMENT;

/// The three reserved `FLG` bits, bits 5 to 7: `0xe0`.
///
/// "Reserved FLG bits must be zero" (`doc/rfc1952.txt` L362), and a decompressor "must give an
/// error indication if any reserved bit is non-zero, since such a bit could indicate the presence
/// of a new field that would cause subsequent data to be interpreted incorrectly" (L477-L480).
///
/// `inflate.c` enforces exactly that, one byte to the left: L557 assembles `CM` and `FLG` into a
/// single 16-bit `state->flags` with `FLG` in the high byte, so its test at L563 reads
/// `if (state->flags & 0xe000)` and fails with "unknown header flags set". `0xe000` is this mask
/// shifted by eight, and the two agree by construction.
pub(crate) const FLG_RESERVED: u8 = 0xe0;

/// The smallest flag byte that has a reserved bit set: `32`, that is `0x20`, bit 5.
///
/// This is the literal `32` of `gzread.c` L153. Keeping it named rather than inline makes the
/// fourth term of [`looks_like_gzip`] self-describing while leaving the comparison in exactly the
/// shape the C source uses, and the invariant below proves the comparison equivalent to a
/// [`FLG_RESERVED`] mask test for all 256 possible flag bytes.
const FLG_RESERVED_FLOOR: u8 = 32;

/// The `MTIME` value that means "no timestamp is available": `0`.
///
/// `MTIME` is a 32-bit little-endian count of seconds since the Unix epoch, and "MTIME = 0 means no
/// time stamp is available" -- `doc/rfc1952.txt` L364-L372. `deflate.c` L1074-L1077 writes four
/// zero bytes whenever the caller supplied no `gz_header`, which is every member the `gzFile` write
/// path produces, since `gz_init` never calls `deflateSetHeader`.
pub(crate) const MTIME_NONE: u32 = 0;

/// The `XFL` value that expresses no preference: `0`.
///
/// Not named by the RFC, which says only that a compliant compressor may leave the non-essential
/// fixed-header fields at "0 for all others" (`doc/rfc1952.txt` L465-L467). It is the third arm of
/// the conditional at `deflate.c` L1078-L1080, taken for the middle levels 2 through 8 with a
/// strategy below `Z_HUFFMAN_ONLY`.
pub(crate) const XFL_UNSPECIFIED: u8 = 0;

/// The `XFL` value meaning "maximum compression, slowest algorithm": `2`.
///
/// `doc/rfc1952.txt` L379-L380. `deflate.c` L1078 writes it when, and only when, the level is 9.
pub(crate) const XFL_MAX_COMPRESSION: u8 = 2;

/// The `XFL` value meaning "fastest algorithm": `4`.
///
/// `doc/rfc1952.txt` L381. `deflate.c` L1079-L1080 writes it when the strategy is
/// `Z_HUFFMAN_ONLY` or beyond, or the level is below 2.
pub(crate) const XFL_FASTEST: u8 = 4;

/// The `OS` value meaning "unknown": `255`.
///
/// `doc/rfc1952.txt` L413 closes a list of fourteen defined file-system codes with `255 - unknown`,
/// and L466 names it the compliant default.
///
/// The value this library writes is **build-dependent and is not this one on a typical build**:
/// `deflate.c` L1081 emits `OS_CODE`, which `zutil.h` defines per platform and defaults to `3`,
/// Unix, at L187-L188. A gzip member produced by `gzip(1)` on Linux likewise carries `3`. So this
/// constant is the vocabulary for "unspecified", not a value to assert against a produced member.
pub(crate) const OS_UNKNOWN: u8 = 255;

/// Length in bytes of the fixed part of a member header: `10`.
///
/// `ID1 ID2 CM FLG` then `MTIME` then `XFL OS`, per the diagram at `doc/rfc1952.txt` L242-L244.
/// Optional fields, when present, follow these ten bytes; the compressed blocks follow those.
pub(crate) const HEADER_LEN: usize = 10;

/// Length in bytes of a member trailer: `8`.
///
/// `CRC32` then `ISIZE`, per `doc/rfc1952.txt` L274-L277, both little-endian.
pub(crate) const TRAILER_LEN: usize = 8;

/// Offset of `CRC32` within a member trailer: `0`.
///
/// A CRC-32 of the *uncompressed* data, "computed according to CRC-32 algorithm used in the ISO
/// 3309 standard and in section 8.1.1.6.2 of ITU-T recommendation V.42"
/// (`doc/rfc1952.txt` L419-L426). `crate::crc32::crc32` computes exactly that value, and
/// `deflate.c` L1269-L1272 is what writes it, low byte first.
pub(crate) const TRAILER_CRC32_OFFSET: usize = 0;

/// Offset of `ISIZE` within a member trailer: `4`.
///
/// "the size of the original (uncompressed) input data modulo 2^32"
/// (`doc/rfc1952.txt` L427-L429). The modulus is not a detail to be tidied away: a member carrying
/// four gibibytes or more stores a truncated size, and a reader that treats `ISIZE` as an exact
/// length will reject perfectly valid input. `deflate.c` L1273-L1276 writes it from `total_in` one
/// byte at a time, which truncates by construction, and `inflate.c` L1097-L1104 compares it against
/// `state->total & 0xffffffff` for the same reason, failing with "incorrect length check".
pub(crate) const TRAILER_ISIZE_OFFSET: usize = 4;

/// Length in bytes of the `XLEN` field that introduces the extra field: `2`.
///
/// `doc/rfc1952.txt` L248-L250 and L415-L417. `XLEN` counts the subfield bytes that follow it and
/// does not include itself.
pub(crate) const XLEN_LEN: usize = 2;

/// Length in bytes of the header of one `FEXTRA` subfield: `4`.
///
/// A subfield is `SI1 SI2 LEN` followed by `LEN` bytes of data (`doc/rfc1952.txt` L437-L439), and
/// "LEN gives the length of the subfield data, excluding the 4 initial bytes" (L459-L460). So a
/// subfield occupies `SUBFIELD_HEADER_LEN + LEN` bytes of the `XLEN` total.
pub(crate) const SUBFIELD_HEADER_LEN: usize = 4;

/// Number of leading bytes [`looks_like_gzip`] inspects: `4`.
///
/// `gz_look` needs four bytes before it can choose between decompressing and copying, and it says
/// so twice: it returns early at `gzread.c` L143-L146 when a non-blocking read has stalled with
/// fewer than four bytes buffered, and its test at L151 begins `strm->avail_in > 3`. The four bytes
/// are `ID1 ID2 CM FLG`, that is the fixed header with `MTIME`, `XFL` and `OS` removed, which is
/// what the invariant below states.
pub(crate) const DETECT_PREFIX_LEN: usize = 4;

// Each block below states a relationship between two things that are separately transcribed from
// `doc/rfc1952.txt` or from the C sources, so that a slip in either transcription is a build failure
// rather than a wrong byte on the wire. They are `const _` items, so they are checked in every
// configuration, test build or not, and they compile to nothing.

/// The byte-at-a-time magic test and the word-at-a-time one are the same test.
///
/// [`ID1`] and [`ID2`] come from `doc/rfc1952.txt` L291-L293 and are what `gzread.c` L152 compares
/// byte by byte; [`MAGIC_LE`] is the `0x8b1f` that `inflate.c` L513 compares after loading sixteen
/// bits, low byte first. If the two ever disagreed, the file layer and the engine would accept
/// different files.
const _: () = assert!(
    MAGIC_LE == u16::from_le_bytes([ID1, ID2]),
    "inflate.c L513: hold == 0x8b1f is ID1 then ID2, little-endian"
);

/// The five defined `FLG` bits and the three reserved ones tile the flag byte exactly once.
///
/// `doc/rfc1952.txt` L303-L310 assigns bits 0 to 4 and reserves bits 5 to 7. Checking the partition
/// catches a duplicated or omitted bit value in the five constants above, which a hand-written
/// [`FLG_KNOWN`] would not.
// Compile-time contract, not a runtime check: `assert!` inside `const _: () = { ... }` is
// evaluated by the compiler, so a violation is a build failure rather than a test failure. Every
// operand is a `const`, which is the point; clippy 0.1.80's `assertions_on_constants` reads that
// as a no-op assertion, so it is relaxed here. Current stable exempts const contexts already.
#[allow(clippy::assertions_on_constants)]
const _: () = {
    assert!(
        FLG_KNOWN == 0x1f,
        "doc/rfc1952.txt L303-L307: FTEXT, FHCRC, FEXTRA, FNAME and FCOMMENT are bits 0 to 4"
    );
    assert!(
        FLG_KNOWN & FLG_RESERVED == 0,
        "doc/rfc1952.txt L308-L310: the reserved bits are disjoint from the defined ones"
    );
    assert!(
        FLG_KNOWN | FLG_RESERVED == u8::MAX,
        "doc/rfc1952.txt L301-L310: the flag byte has exactly eight bits and all are accounted for"
    );
};

/// `gzread.c` L153's `< 32` and a [`FLG_RESERVED`] mask test are one predicate, for all 256 bytes.
///
/// This is the equivalence that lets [`looks_like_gzip`] be read either as a transcription of the C
/// source or as a statement about reserved bits, and it is checked exhaustively rather than argued:
/// the loop visits every value a `u8` can take. `gz_look`'s choice between decompressing and copying
/// is caller-visible through `gzdirect`, so a predicate that differed from the C one on even one
/// byte value would be a behavioural change.
const _: () = {
    let mut flg: u8 = 0;
    loop {
        assert!(
            (flg < FLG_RESERVED_FLOOR) == (flg & FLG_RESERVED == 0),
            "gzread.c L153: next_in[3] < 32 rejects exactly the flag bytes with a reserved bit set"
        );
        if flg == u8::MAX {
            break;
        }
        flg += 1;
    }
};

/// The fixed header is `ID1 ID2 CM FLG`, then `MTIME`, then `XFL OS`, and nothing else.
///
/// `doc/rfc1952.txt` L242-L244. Written as a sum of the field widths so that [`HEADER_LEN`] cannot
/// drift from the diagram, and so that the four-byte detection prefix is visibly the part of the
/// header that precedes `MTIME` -- which is what `gzread.c` L151-L153 inspects.
// Compile-time contract, not a runtime check: `assert!` inside `const _: () = { ... }` is
// evaluated by the compiler, so a violation is a build failure rather than a test failure. Every
// operand is a `const`, which is the point; clippy 0.1.80's `assertions_on_constants` reads that
// as a no-op assertion, so it is relaxed here. Current stable exempts const contexts already.
#[allow(clippy::assertions_on_constants)]
const _: () = {
    assert!(
        HEADER_LEN == DETECT_PREFIX_LEN + U32_LEN + 1 + 1,
        "doc/rfc1952.txt L242-L244: ID1 ID2 CM FLG, MTIME, XFL, OS"
    );
    assert!(
        DETECT_PREFIX_LEN == HEADER_LEN - U32_LEN - 2,
        "gzread.c L151-L153: the detection prefix is the header before MTIME"
    );
};

/// The trailer is `CRC32` then `ISIZE`, each a little-endian 32-bit field, in that order.
///
/// `doc/rfc1952.txt` L274-L277. Checking that the two offsets and the two widths tile
/// [`TRAILER_LEN`] catches a swapped pair of offsets, which is the mistake that would otherwise be
/// caught only by a reader rejecting the produced file.
// Compile-time contract, not a runtime check: `assert!` inside `const _: () = { ... }` is
// evaluated by the compiler, so a violation is a build failure rather than a test failure. Every
// operand is a `const`, which is the point; clippy 0.1.80's `assertions_on_constants` reads that
// as a no-op assertion, so it is relaxed here. Current stable exempts const contexts already.
#[allow(clippy::assertions_on_constants)]
const _: () = {
    assert!(
        TRAILER_CRC32_OFFSET == 0,
        "doc/rfc1952.txt L274-L277: CRC32 opens the trailer"
    );
    assert!(
        TRAILER_ISIZE_OFFSET == TRAILER_CRC32_OFFSET + U32_LEN,
        "doc/rfc1952.txt L274-L277: ISIZE follows CRC32 immediately"
    );
    assert!(
        TRAILER_LEN == TRAILER_ISIZE_OFFSET + U32_LEN,
        "doc/rfc1952.txt L274-L277: the trailer is those two fields and nothing more"
    );
};

/// The extra field is a two-byte `XLEN` followed by subfields of `SI1 SI2 LEN` plus `LEN` bytes.
///
/// `doc/rfc1952.txt` L415-L417 for `XLEN` and L437-L439 with L459-L460 for the subfield header,
/// whose four bytes are the two identifier bytes plus a 16-bit length.
// Compile-time contract, not a runtime check: `assert!` inside `const _: () = { ... }` is
// evaluated by the compiler, so a violation is a build failure rather than a test failure. Every
// operand is a `const`, which is the point; clippy 0.1.80's `assertions_on_constants` reads that
// as a no-op assertion, so it is relaxed here. Current stable exempts const contexts already.
#[allow(clippy::assertions_on_constants)]
const _: () = {
    assert!(
        XLEN_LEN == U16_LEN,
        "doc/rfc1952.txt L415-L417: XLEN is a 16-bit little-endian count"
    );
    assert!(
        SUBFIELD_HEADER_LEN == 2 + U16_LEN,
        "doc/rfc1952.txt L437-L439: SI1, SI2 and a 16-bit LEN precede the subfield data"
    );
};

/// `MTIME` is a 32-bit field, and the three `XFL` values the reference writes are distinguishable.
///
/// `doc/rfc1952.txt` L364-L372 for `MTIME`; L374-L381 and `deflate.c` L1078-L1080 for the three arms
/// of the `XFL` conditional. `OS` is a single byte whose "unknown" value is the largest one it can
/// hold, which is what makes `255` a safe sentinel (`doc/rfc1952.txt` L399-L413).
// Compile-time contract, not a runtime check: `assert!` inside `const _: () = { ... }` is
// evaluated by the compiler, so a violation is a build failure rather than a test failure. Every
// operand is a `const`, which is the point; clippy 0.1.80's `assertions_on_constants` reads that
// as a no-op assertion, so it is relaxed here. Current stable exempts const contexts already.
#[allow(clippy::assertions_on_constants)]
const _: () = {
    assert!(
        MTIME_NONE.to_le_bytes().len() == U32_LEN,
        "doc/rfc1952.txt L364-L372: MTIME is a 32-bit little-endian Unix time"
    );
    assert!(
        XFL_UNSPECIFIED != XFL_MAX_COMPRESSION
            && XFL_MAX_COMPRESSION != XFL_FASTEST
            && XFL_UNSPECIFIED != XFL_FASTEST,
        "deflate.c L1078-L1080: the three arms of the XFL conditional produce distinct values"
    );
    assert!(
        OS_UNKNOWN == u8::MAX,
        "doc/rfc1952.txt L413: 255 - unknown closes the list of file-system codes"
    );
};

/// Does `prefix` begin with four bytes consistent with a gzip member header?
///
/// The test at `gzread.c` L151-L153, reproduced term for term and in the same order: more than
/// three bytes available, then `31`, `139`, `8`, and a fourth byte below `32`.
///
/// # What the caller does with the answer
///
/// `gz_look` uses it, and only it, to decide between decompressing and copying. On `true` it resets
/// the inflate stream and sets `state->how = GZIP` (`gzread.c` L154-L158); on `false` it copies the
/// buffered bytes straight through and sets `state->how = COPY` (L164-L168). That choice is visible
/// to the application afterwards through `gzdirect`, so this predicate is part of the library's
/// observable behaviour and not an internal optimisation.
///
/// # It is a heuristic on four bytes, not a validation
///
/// Deliberately so. It looks at the identification bytes, the compression method and the flag byte,
/// and at nothing else -- not `MTIME`, not `XFL`, not `OS`, not the optional fields, not the
/// trailer, and not the compressed blocks. A four-byte prefix that passes may still turn out to be
/// a corrupt or truncated member; the authoritative checks happen afterwards, in the engine, which
/// re-tests the magic word at `inflate.c` L513, the compression method at L562 and the reserved
/// flag bits at L563, and which fails with `Z_DATA_ERROR` if any of them is wrong.
///
/// Two consequences follow from that division, and both are load-bearing:
///
/// * **Neither a stricter nor a looser test is admissible.** Widening it would send input to the
///   inflate engine that the reference implementation copies transparently; narrowing it would copy
///   input the reference decompresses. Either way `gzdirect` starts answering differently for the
///   same file, and `test/minigzip.c`, which round-trips through this path, would diverge from the
///   reference build.
/// * **The fourth term is about reserved bits.** `flg < 32` is exactly `flg & FLG_RESERVED == 0`
///   for a byte -- proven exhaustively by the invariant above -- so the test rejects any member
///   whose flag byte sets bit 5, 6 or 7, in keeping with `doc/rfc1952.txt` L362 and L477-L480. It
///   accepts every combination of the five defined bits, including all of them at once
///   (`FLG = 0x1f = 31`).
///
/// # Short input is `false`, not an error
///
/// Fewer than [`DETECT_PREFIX_LEN`] bytes answers `false`, mirroring the `strm->avail_in > 3` term.
/// That is not the same as "this is not gzip", and the difference is the caller's to make: deciding
/// between "not gzip" and "come back later" needs the file state, which this function does not have.
/// `gz_look` separates the two before reaching here, returning early at `gzread.c` L143-L146 when a
/// non-blocking read stalled with fewer than four bytes.
#[must_use]
pub(crate) fn looks_like_gzip(prefix: &[u8]) -> bool {
    // `gzread.c` L151's `strm->avail_in > 3`, spelled as a fallible fixed-size borrow so that the
    // four comparisons below need no bound check of their own. `first_chunk` yields `None` for a
    // short slice and ignores anything past the fourth byte, which is what the C code does too:
    // `avail_in` is usually a whole buffer, and only these four bytes are examined.
    let Some(&[id1, id2, cm, flg]) = prefix.first_chunk::<DETECT_PREFIX_LEN>() else {
        return false;
    };

    // `gzread.c` L152-L153, in order and unchanged. The fourth term keeps the C source's `< 32`
    // spelling; the exhaustive invariant above establishes that it is a reserved-bit test.
    id1 == ID1 && id2 == ID2 && cm == CM_DEFLATE && flg < FLG_RESERVED_FLOOR
}

/// Read a little-endian 16-bit member field from the start of `bytes`.
///
/// The fields of this width are `XLEN`, a `FEXTRA` subfield's `LEN` and the optional `CRC16`
/// (`doc/rfc1952.txt` L415-L417, L437-L439 and L266-L268). Returns `None` if `bytes` is shorter than
/// [`U16_LEN`]; ignores anything beyond the second byte, so a caller may pass a whole buffer and let
/// the field be read from its front.
///
/// The C sources assemble these fields a byte at a time -- `deflate.c` L1107-L1108 writes `XLEN` low
/// byte first, and `inflate.c`'s `NEEDBITS(16)` accumulates from the low bit upwards -- which is the
/// same order [`u16::from_le_bytes`] applies.
pub(crate) fn u16_le(bytes: &[u8]) -> Option<u16> {
    Some(u16::from_le_bytes(*bytes.first_chunk::<U16_LEN>()?))
}

/// Read a little-endian 32-bit member field from the start of `bytes`.
///
/// The fields of this width are `MTIME` in the header and `CRC32` and `ISIZE` in the trailer
/// (`doc/rfc1952.txt` L364-L372 and L274-L277). Returns `None` if `bytes` is shorter than
/// [`U32_LEN`]; ignores anything beyond the fourth byte.
///
/// To read a trailer, offset the slice by [`TRAILER_CRC32_OFFSET`] or [`TRAILER_ISIZE_OFFSET`]
/// first. Note that a value read from the `ISIZE` position is the uncompressed length *modulo
/// 2^32*, never necessarily the length itself (`doc/rfc1952.txt` L427-L429).
pub(crate) fn u32_le(bytes: &[u8]) -> Option<u32> {
    Some(u32::from_le_bytes(*bytes.first_chunk::<U32_LEN>()?))
}

/// Write `value` as a little-endian 16-bit member field at the start of `out`.
///
/// Returns `None`, having written nothing, if `out` is shorter than [`U16_LEN`]; otherwise writes
/// exactly two bytes and leaves the rest of `out` untouched. See [`u16_le`] for the fields this
/// applies to and for the byte order the C sources use.
pub(crate) fn write_u16_le(out: &mut [u8], value: u16) -> Option<()> {
    *out.first_chunk_mut::<U16_LEN>()? = value.to_le_bytes();
    Some(())
}

/// Write `value` as a little-endian 32-bit member field at the start of `out`.
///
/// Returns `None`, having written nothing, if `out` is shorter than [`U32_LEN`]; otherwise writes
/// exactly four bytes and leaves the rest of `out` untouched. See [`u32_le`] for the fields this
/// applies to.
///
/// For `ISIZE`, `value` is the uncompressed length reduced modulo 2^32
/// (`doc/rfc1952.txt` L427-L429); `deflate.c` L1273-L1276 performs that reduction implicitly by
/// writing the low four bytes of `total_in`.
pub(crate) fn write_u32_le(out: &mut [u8], value: u32) -> Option<()> {
    *out.first_chunk_mut::<U32_LEN>()? = value.to_le_bytes();
    Some(())
}

/// The `CRC16` value that `FLG.FHCRC` announces, computed over `header_bytes`.
///
/// "If FHCRC is set, a CRC16 for the gzip header is present, immediately before the compressed data.
/// The CRC16 consists of the two least significant bytes of the CRC32 for all bytes of the gzip
/// header up to and not including the CRC16" -- `doc/rfc1952.txt` L325-L328.
///
/// `header_bytes` must therefore be the whole header so far: the ten fixed bytes plus whichever of
/// the extra field, the file name and the comment are present, and nothing after them. This function
/// cannot check that for the caller -- the header's length depends on the flag byte and on where the
/// two terminators fall -- so getting the extent right is the caller's obligation, and it is the only
/// obligation this function has.
///
/// # Fidelity
///
/// The check value comes from [`crate::crc32::crc32`], the Rust counterpart of `crc32.c`, seeded with zero, which
/// is the same entry point and the same seed both C sides use: `deflate.c` L1110-L1112 runs
/// `crc32_z` over the pending buffer once the header has been staged, and `inflate.c` accumulates
/// over each field as it is parsed (L516 for the seed, then L517, L571, L580, L591, L602, L623, L645
/// and L667 for the pieces) before comparing at L679. CRC-32 is never reimplemented here.
///
/// Taking the low two bytes is a truncation, and it is done through
/// [`u32::to_le_bytes`] rather than through a narrowing cast: the two least significant bytes of the
/// check value, in the order the field occupies on the wire, are literally the first two bytes of its
/// little-endian encoding. That spelling is both closer to the RFC's wording and free of a cast whose
/// range would have to be argued.
#[must_use]
pub(crate) fn header_crc16(header_bytes: &[u8]) -> u16 {
    // Two of these four bytes are the CRC16 field; the other two are discarded, which is what
    // "the two least significant bytes" means and what `& 0xffff` does at `inflate.c` L679.
    let [low, high, _, _] = crc32(0, header_bytes).to_le_bytes();

    u16::from_le_bytes([low, high])
}

// Fixture indexing: every index below is a literal into a fixture this module just built,
// so each one is provably in range. `clippy::indexing_slicing` is denied workspace-wide and
// is relaxed HERE ONLY, on the test module -- not through a clippy.toml key, which would be a
// field the 1.80 floor does not recognise and would abort the whole lint run.
#[allow(clippy::indexing_slicing)]
#[cfg(test)]
mod tests {
    use super::{
        header_crc16, looks_like_gzip, u16_le, u32_le, write_u16_le, write_u32_le, CM_DEFLATE,
        DETECT_PREFIX_LEN, FLG_FCOMMENT, FLG_FEXTRA, FLG_FHCRC, FLG_FNAME, FLG_FTEXT, FLG_KNOWN,
        FLG_RESERVED, FLG_RESERVED_FLOOR, HEADER_LEN, ID1, ID2, MAGIC_LE, MTIME_NONE, OS_UNKNOWN,
        SUBFIELD_HEADER_LEN, TRAILER_CRC32_OFFSET, TRAILER_ISIZE_OFFSET, TRAILER_LEN, U16_LEN,
        U32_LEN, XFL_FASTEST, XFL_MAX_COMPRESSION, XFL_UNSPECIFIED, XLEN_LEN,
    };

    /// The reflected CRC-32 polynomial, `#define POLY 0xedb88320` at `crc32.c` L157.
    ///
    /// Restated here rather than imported so that the reference below is independent of the
    /// transcribed tables the production path uses; agreement between the two then means something.
    const POLY: u32 = 0xedb8_8320;

    /// A complete gzip member: the output of `printf 'hello' | gzip -c` on this machine.
    ///
    /// Captured with `od -An -tx1` and pinned here as a fixture, so the assertions below are made
    /// against a member produced by `gzip(1)` itself rather than against this implementation's own output.
    /// Its shape is the diagram in the module documentation: ten fixed header bytes with no optional
    /// fields (`FLG` is zero), seven bytes of compressed blocks, then the eight-byte trailer.
    ///
    /// The middle seven bytes are the only part that a different compressor version might spell
    /// differently, and nothing below depends on them.
    const GZIP_HELLO: [u8; 25] = [
        0x1f, 0x8b, 0x08, 0x00, // ID1 ID2 CM FLG
        0x00, 0x00, 0x00, 0x00, // MTIME = 0, no timestamp
        0x00, 0x03, // XFL = 0, OS = 3 (Unix)
        0xcb, 0x48, 0xcd, 0xc9, 0xc9, 0x07, 0x00, // compressed blocks
        0x86, 0xa6, 0x10, 0x36, // CRC32 = 0x3610a686, little-endian
        0x05, 0x00, 0x00, 0x00, // ISIZE = 5, little-endian
    ];

    /// The payload of [`GZIP_HELLO`], and the preset dictionary of `test/example.c` L40.
    const HELLO: &[u8] = b"hello";

    /// The highest operating-system code `doc/rfc1952.txt` L399-L412 assigns: `13`, Acorn RISCOS.
    ///
    /// Values above it are unassigned apart from [`OS_UNKNOWN`] at L413, so a produced member's `OS`
    /// byte can be required to be at most this or exactly that sentinel -- and nothing narrower,
    /// because `deflate.c` L1081 writes a build-dependent code.
    const OS_HIGHEST_DEFINED: u8 = 13;

    /// CRC-32 computed from [`POLY`] alone: no table, no shared code with the production path.
    ///
    /// `crc32.c` L635 pre-conditions with `(~crc) & 0xffffffff` and L940 post-conditions with
    /// `crc ^ 0xffffffff`; both are `!` on a `u32`, and both are applied here so that the result is
    /// directly comparable to a published CRC-32 value.
    fn bitwise_crc32(bytes: &[u8]) -> u32 {
        let mut crc = !0u32;
        for &byte in bytes {
            crc ^= u32::from(byte);
            for _ in 0..8 {
                crc = if crc & 1 == 0 {
                    crc >> 1
                } else {
                    (crc >> 1) ^ POLY
                };
            }
        }
        !crc
    }

    /// The two least significant bytes of `value`, spelled as the mask-and-narrow the RFC implies.
    ///
    /// `doc/rfc1952.txt` L326-L328. Deliberately a different spelling from the production one, which
    /// slices the little-endian encoding instead, so that the two cannot fail in the same way.
    /// `try_from` cannot fail after the mask, and unwrapping in a test is permitted by clippy.toml.
    fn low_16(value: u32) -> u16 {
        u16::try_from(value & 0xffff).unwrap()
    }

    /// A four-byte detection prefix with the reference identification bytes and the given flag byte.
    fn prefix_with_flg(flg: u8) -> [u8; DETECT_PREFIX_LEN] {
        [ID1, ID2, CM_DEFLATE, flg]
    }

    /// A ten-byte fixed header carrying `flg` and `os`, no timestamp and no `XFL` preference.
    ///
    /// The shape of `doc/rfc1952.txt` L242-L244, and of what `deflate.c` L1069-L1081 emits.
    fn fixed_header(flg: u8, os: u8) -> [u8; HEADER_LEN] {
        [ID1, ID2, CM_DEFLATE, flg, 0, 0, 0, 0, XFL_UNSPECIFIED, os]
    }

    /// `doc/rfc1952.txt` L291-L299 and `inflate.c` L513: the identification bytes and the method.
    #[test]
    fn identification_and_method_match_the_rfc() {
        assert_eq!(ID1, 0x1f);
        assert_eq!(ID2, 0x8b);
        assert_eq!(CM_DEFLATE, 8);

        // The word form `inflate` matches, and its two halves, are the same two bytes.
        assert_eq!(MAGIC_LE, 0x8b1f);
        assert_eq!(MAGIC_LE.to_le_bytes(), [ID1, ID2]);
    }

    /// `doc/rfc1952.txt` L303-L310: five defined flag bits, three reserved ones, one byte.
    #[test]
    fn flag_bits_match_the_rfc() {
        assert_eq!(FLG_FTEXT, 1 << 0);
        assert_eq!(FLG_FHCRC, 1 << 1);
        assert_eq!(FLG_FEXTRA, 1 << 2);
        assert_eq!(FLG_FNAME, 1 << 3);
        assert_eq!(FLG_FCOMMENT, 1 << 4);

        // Bits 5, 6 and 7, which a compliant member never sets (RFC L362).
        assert_eq!(FLG_RESERVED, (1 << 5) | (1 << 6) | (1 << 7));
        assert_eq!(FLG_RESERVED, 0xe0);

        // The defined bits, and the partition of the byte.
        assert_eq!(FLG_KNOWN, 0x1f);
        assert_eq!(FLG_KNOWN & FLG_RESERVED, 0);
        assert_eq!(FLG_KNOWN | FLG_RESERVED, u8::MAX);

        // `inflate.c` L557 puts FLG in the high byte of a 16-bit word, so its reserved-bit test at
        // L563 reads `flags & 0xe000`. The two masks must be the same mask.
        assert_eq!(u16::from(FLG_RESERVED) << 8, 0xe000);

        // The floor is the lowest value with a reserved bit set: `gzread.c` L153's literal 32.
        assert_eq!(FLG_RESERVED_FLOOR, 32);
        assert_eq!(FLG_RESERVED_FLOOR, FLG_KNOWN + 1);
    }

    /// `doc/rfc1952.txt` L364-L381 and L399-L413: `MTIME`, `XFL` and `OS` sentinels.
    #[test]
    fn mtime_xfl_and_os_sentinels_match_the_rfc() {
        // RFC L371-L372: "MTIME = 0 means no time stamp is available".
        assert_eq!(MTIME_NONE, 0);
        assert_eq!(MTIME_NONE.to_le_bytes(), [0; 4]);

        // RFC L379-L381, and the three arms of `deflate.c` L1078-L1080.
        assert_eq!(XFL_UNSPECIFIED, 0);
        assert_eq!(XFL_MAX_COMPRESSION, 2);
        assert_eq!(XFL_FASTEST, 4);

        // RFC L413: 255 - unknown. Not the value this library writes: `deflate.c` L1081 emits
        // `OS_CODE`, which `zutil.h` L187-L188 defaults to 3, so no test asserts a produced OS byte.
        assert_eq!(OS_UNKNOWN, 255);
        assert_eq!(OS_UNKNOWN, u8::MAX);
    }

    /// `doc/rfc1952.txt` L242-L277 and L415-L460: the field widths and offsets tile as documented.
    #[test]
    fn lengths_and_offsets_match_the_rfc() {
        assert_eq!(U16_LEN, 2);
        assert_eq!(U32_LEN, 4);

        // The fixed header: ID1 ID2 CM FLG, MTIME, XFL, OS.
        assert_eq!(HEADER_LEN, 10);
        assert_eq!(HEADER_LEN, DETECT_PREFIX_LEN + U32_LEN + 1 + 1);

        // The trailer: CRC32 then ISIZE.
        assert_eq!(TRAILER_LEN, 8);
        assert_eq!(TRAILER_CRC32_OFFSET, 0);
        assert_eq!(TRAILER_ISIZE_OFFSET, 4);
        assert_eq!(TRAILER_ISIZE_OFFSET, TRAILER_CRC32_OFFSET + U32_LEN);
        assert_eq!(TRAILER_LEN, TRAILER_ISIZE_OFFSET + U32_LEN);

        // The extra field: a 16-bit XLEN, then subfields headed by SI1, SI2 and a 16-bit LEN.
        assert_eq!(XLEN_LEN, 2);
        assert_eq!(SUBFIELD_HEADER_LEN, 4);
        assert_eq!(SUBFIELD_HEADER_LEN, 2 + U16_LEN);

        // `gzread.c` L151-L153 inspects exactly the four bytes that precede MTIME.
        assert_eq!(DETECT_PREFIX_LEN, 4);
        assert_eq!(DETECT_PREFIX_LEN, HEADER_LEN - U32_LEN - 2);
    }

    /// The two prefixes at the extremes of the accepted flag range, per `gzread.c` L151-L153.
    #[test]
    fn looks_like_gzip_accepts_the_reference_prefixes() {
        // No optional fields at all -- what `deflate.c` L1073 writes and what `gzip(1)` produced.
        assert!(looks_like_gzip(&[31, 139, 8, 0]));

        // Every defined flag bit set at once: FLG = 0x1f = 31, the largest accepted value.
        assert!(looks_like_gzip(&[31, 139, 8, 31]));
    }

    /// One wrong byte in any of the four positions is a rejection, per `gzread.c` L152-L153.
    #[test]
    fn looks_like_gzip_rejects_a_wrong_byte_in_any_position() {
        // A reserved flag bit (bit 5) is set: the first rejected flag byte.
        assert!(!looks_like_gzip(&[31, 139, 8, 32]));

        // CM = 9: reserved by RFC L294-L299, and rejected by `inflate.c` L562 as well.
        assert!(!looks_like_gzip(&[31, 139, 9, 0]));

        // ID2 and ID1 wrong by one.
        assert!(!looks_like_gzip(&[31, 138, 8, 0]));
        assert!(!looks_like_gzip(&[30, 139, 8, 0]));
    }

    /// Fewer than four bytes answers `false`, mirroring the `strm->avail_in > 3` term.
    #[test]
    fn looks_like_gzip_rejects_every_short_prefix() {
        let member = GZIP_HELLO;

        for len in 0..DETECT_PREFIX_LEN {
            assert!(
                !looks_like_gzip(&member[..len]),
                "a {len}-byte prefix cannot be decided yet, so it must not answer true"
            );
        }

        // Explicitly, the four truncations of the real member's magic.
        assert!(!looks_like_gzip(&[]));
        assert!(!looks_like_gzip(&[31]));
        assert!(!looks_like_gzip(&[31, 139]));
        assert!(!looks_like_gzip(&[31, 139, 8]));

        // And the fourth byte is what tips it over.
        assert!(looks_like_gzip(&member[..DETECT_PREFIX_LEN]));
    }

    /// Bytes past the fourth are ignored: `gz_look` tests a prefix of a whole input buffer.
    #[test]
    fn looks_like_gzip_ignores_bytes_beyond_the_fourth() {
        assert!(looks_like_gzip(&GZIP_HELLO));
        assert!(looks_like_gzip(&GZIP_HELLO[..HEADER_LEN]));

        // Trailing junk of any shape cannot change the answer.
        assert!(looks_like_gzip(&[31, 139, 8, 0, 0xff, 0xff, 0xff]));
        assert!(!looks_like_gzip(&[31, 139, 8, 0xe0, 31, 139, 8, 0]));
    }

    /// Exhaustive over the flag byte: accepted exactly when no reserved bit is set.
    ///
    /// This is the behavioural statement that `gzread.c` L153's `< 32` makes, checked for all 256
    /// values rather than at the boundary, because the accept/reject split is observable through
    /// `gzdirect` and must not move by even one value.
    #[test]
    fn looks_like_gzip_accepts_exactly_the_unreserved_flag_bytes() {
        for flg in 0..=u8::MAX {
            let expected = flg & FLG_RESERVED == 0;

            assert_eq!(
                looks_like_gzip(&prefix_with_flg(flg)),
                expected,
                "flag byte {flg:#04x} must be {}",
                if expected { "accepted" } else { "rejected" }
            );
            assert_eq!(expected, flg < FLG_RESERVED_FLOOR);
        }
    }

    /// Exhaustive over the compression method: accepted only for `CM = 8`.
    #[test]
    fn looks_like_gzip_accepts_only_the_deflate_method() {
        for cm in 0..=u8::MAX {
            assert_eq!(looks_like_gzip(&[ID1, ID2, cm, 0]), cm == CM_DEFLATE);
        }
    }

    /// Reading takes the leading field, little-endian, and ignores the rest of the slice.
    #[test]
    fn codecs_read_the_leading_field_little_endian() {
        assert_eq!(u16_le(&[0x34, 0x12]), Some(0x1234));
        assert_eq!(u16_le(&[0x34, 0x12, 0xff, 0xff]), Some(0x1234));
        assert_eq!(u32_le(&[0x78, 0x56, 0x34, 0x12]), Some(0x1234_5678));
        assert_eq!(u32_le(&[0x78, 0x56, 0x34, 0x12, 0x00]), Some(0x1234_5678));

        // Byte order is the property that matters: the low byte comes first on the wire.
        assert_eq!(u16_le(&[0x00, 0x01]), Some(0x0100));
        assert_eq!(u32_le(&[0x00, 0x00, 0x00, 0x01]), Some(0x0100_0000));
    }

    /// A slice shorter than the field yields `None`, at every short length.
    #[test]
    fn codecs_reject_short_slices() {
        let bytes = [0xaa_u8; 8];

        for len in 0..U16_LEN {
            assert_eq!(u16_le(&bytes[..len]), None);
        }
        for len in 0..U32_LEN {
            assert_eq!(u32_le(&bytes[..len]), None);
        }
        assert!(u16_le(&bytes[..U16_LEN]).is_some());
        assert!(u32_le(&bytes[..U32_LEN]).is_some());

        let mut out = [0xaa_u8; 8];
        for len in 0..U16_LEN {
            assert_eq!(write_u16_le(&mut out[..len], 0x1234), None);
        }
        for len in 0..U32_LEN {
            assert_eq!(write_u32_le(&mut out[..len], 0x1234_5678), None);
        }

        // A refused write leaves the buffer exactly as it was.
        assert_eq!(out, [0xaa; 8]);
    }

    /// Writing then reading returns the value, for both widths and across the whole value range.
    #[test]
    fn codecs_round_trip() {
        let mut buffer = [0u8; 4];

        for value in 0..=u16::MAX {
            assert_eq!(write_u16_le(&mut buffer, value), Some(()));
            assert_eq!(u16_le(&buffer), Some(value));
        }

        // A 32-bit sweep would be gratuitous; these are the values that break byte order or
        // sign-extension mistakes, plus a stride that visits every byte lane.
        let interesting = [
            0,
            1,
            0xff,
            0x100,
            0xffff,
            0x1_0000,
            0x7fff_ffff,
            0x8000_0000,
            0xdead_beef,
            0x3610_a686,
            u32::MAX,
        ];
        for value in interesting {
            assert_eq!(write_u32_le(&mut buffer, value), Some(()));
            assert_eq!(u32_le(&buffer), Some(value));
            assert_eq!(buffer, value.to_le_bytes());
        }
    }

    /// A write touches exactly the field and nothing after it.
    #[test]
    fn codecs_write_only_the_leading_field() {
        let mut buffer = [0x5a_u8; 8];

        assert_eq!(write_u16_le(&mut buffer, 0x1234), Some(()));
        assert_eq!(&buffer[..U16_LEN], &[0x34, 0x12]);
        assert!(buffer[U16_LEN..].iter().all(|&byte| byte == 0x5a));

        let mut buffer = [0x5a_u8; 8];
        assert_eq!(write_u32_le(&mut buffer, 0x1234_5678), Some(()));
        assert_eq!(&buffer[..U32_LEN], &[0x78, 0x56, 0x34, 0x12]);
        assert!(buffer[U32_LEN..].iter().all(|&byte| byte == 0x5a));
    }

    /// The trailer of a member produced by `gzip(1)` decodes to the CRC-32 and length of `hello`.
    ///
    /// This is the end-to-end statement that the constants and codecs describe the real format:
    /// the fixture came out of `gzip(1)`, the check value is recomputed here from the polynomial,
    /// and `ISIZE` is the payload length.
    #[test]
    fn real_gzip_member_trailer_decodes() {
        let trailer = &GZIP_HELLO[GZIP_HELLO.len() - TRAILER_LEN..];

        let crc = u32_le(&trailer[TRAILER_CRC32_OFFSET..]).unwrap();
        let isize_field = u32_le(&trailer[TRAILER_ISIZE_OFFSET..]).unwrap();

        assert_eq!(crc, 0x3610_a686);
        assert_eq!(crc, bitwise_crc32(HELLO));
        assert_eq!(isize_field, u32::try_from(HELLO.len()).unwrap());
    }

    /// The fixed header of that same member matches the documented layout, field by field.
    #[test]
    fn real_gzip_member_header_matches_the_documented_layout() {
        let header = &GZIP_HELLO[..HEADER_LEN];

        assert!(looks_like_gzip(header));
        assert_eq!(header[0], ID1);
        assert_eq!(header[1], ID2);
        assert_eq!(header[2], CM_DEFLATE);

        // No optional field is present, so no reserved bit is set either.
        let flg = header[3];
        assert_eq!(flg, 0);
        assert_eq!(flg & FLG_RESERVED, 0);
        assert_eq!(flg & FLG_KNOWN, 0);

        // `gzip -c` reading from a pipe has no file to take a timestamp from.
        assert_eq!(u32_le(&header[4..]), Some(MTIME_NONE));

        // XFL and OS close the fixed header. The OS byte is deliberately not asserted against one
        // value: `deflate.c` L1081 writes the build-dependent `OS_CODE`, and `gzip(1)` likewise, so
        // all that can be required of it is that it name a code the RFC lists.
        assert_eq!(header[8], XFL_UNSPECIFIED);
        assert!(
            header[9] <= OS_HIGHEST_DEFINED || header[9] == OS_UNKNOWN,
            "the OS byte must be a code from doc/rfc1952.txt L399-L413"
        );

        // The member is the fixed header, some compressed blocks, and the trailer.
        assert!(GZIP_HELLO.len() > HEADER_LEN + TRAILER_LEN);
    }

    /// A trailer written through the codecs at the documented offsets reads back unchanged.
    #[test]
    fn trailer_round_trips_through_the_codecs() {
        let crc = bitwise_crc32(HELLO);
        let isize_field = u32::try_from(HELLO.len()).unwrap();

        let mut trailer = [0u8; TRAILER_LEN];
        assert_eq!(
            write_u32_le(&mut trailer[TRAILER_CRC32_OFFSET..], crc),
            Some(())
        );
        assert_eq!(
            write_u32_le(&mut trailer[TRAILER_ISIZE_OFFSET..], isize_field),
            Some(())
        );

        assert_eq!(trailer, GZIP_HELLO[GZIP_HELLO.len() - TRAILER_LEN..]);
        assert_eq!(u32_le(&trailer[TRAILER_CRC32_OFFSET..]), Some(crc));
        assert_eq!(u32_le(&trailer[TRAILER_ISIZE_OFFSET..]), Some(isize_field));
    }

    /// `ISIZE` is the uncompressed length modulo 2^32, and the codec is what performs the reduction.
    ///
    /// `doc/rfc1952.txt` L427-L429. A member of exactly 2^32 bytes stores zero, which is why a
    /// reader must not treat `ISIZE` as an exact length.
    #[test]
    fn isize_is_the_length_modulo_two_to_the_thirty_second() {
        let mut field = [0u8; U32_LEN];

        for length in [0_u64, 1, 0xffff_ffff, 0x1_0000_0000, 0x1_0000_0001] {
            let reduced = u32::try_from(length % (1_u64 << 32)).unwrap();
            assert_eq!(write_u32_le(&mut field, reduced), Some(()));
            assert_eq!(u32_le(&field), Some(reduced));
        }

        assert_eq!(write_u32_le(&mut field, 0), Some(()));
        assert_eq!(field, [0; U32_LEN]);
    }

    /// The `XLEN` and subfield constants suffice to walk an extra field to its exact end.
    ///
    /// `doc/rfc1952.txt` L431-L461. The walk lives in this test rather than in the module: parsing
    /// the extra field of a stream belongs to `inflate`, which does it at `inflate.c` L595-L631.
    /// What is checked here is only that the vocabulary is sufficient and self-consistent.
    #[test]
    fn extra_field_subfields_tile_the_xlen_total() {
        // Two subfields: "AP" (RFC L457, Apollo file type information) carrying three bytes, and
        // "Zz" carrying none.
        let payload = [
            b'A', b'P', 0x03, 0x00, 0x11, 0x22, 0x33, // SI1 SI2 LEN=3, data
            b'Z', b'z', 0x00, 0x00, // SI1 SI2 LEN=0, no data
        ];

        let mut field = [0u8; XLEN_LEN + 11];
        assert_eq!(
            write_u16_le(&mut field, u16::try_from(payload.len()).unwrap()),
            Some(())
        );
        field[XLEN_LEN..].copy_from_slice(&payload);

        let xlen = usize::from(u16_le(&field).unwrap());
        assert_eq!(xlen, payload.len());
        assert_eq!(field.len(), XLEN_LEN + xlen);

        // Walk the subfields. `LEN` counts the data only, "excluding the 4 initial bytes"
        // (RFC L459-L460), so each subfield occupies SUBFIELD_HEADER_LEN + LEN bytes.
        let mut cursor = XLEN_LEN;
        let mut seen = 0;
        while cursor < XLEN_LEN + xlen {
            let header = &field[cursor..];
            let si = (header[0], header[1]);
            let len = usize::from(u16_le(&header[2..]).unwrap());

            if seen == 0 {
                assert_eq!(si, (b'A', b'P'));
                assert_eq!(len, 3);
                assert_eq!(&header[SUBFIELD_HEADER_LEN..][..len], &[0x11, 0x22, 0x33]);
            } else {
                assert_eq!(si, (b'Z', b'z'));
                assert_eq!(len, 0);
            }

            cursor += SUBFIELD_HEADER_LEN + len;
            seen += 1;
        }

        assert_eq!(seen, 2);
        assert_eq!(
            cursor,
            XLEN_LEN + xlen,
            "the subfields must tile XLEN exactly"
        );
    }

    /// The `FHCRC` value is the low two bytes of the CRC-32 of the header bytes, and nothing else.
    ///
    /// `doc/rfc1952.txt` L325-L328. Checked against a table-free CRC-32 built from the polynomial,
    /// so this test is independent of the transcribed tables as well as of the production code path.
    #[test]
    fn header_crc16_is_the_low_half_of_a_table_free_crc32() {
        // A fixed header with FHCRC announced, which is the shape the value describes.
        let header = fixed_header(FLG_FHCRC, OS_UNKNOWN);

        assert_eq!(header_crc16(&header), low_16(bitwise_crc32(&header)));

        // And over every prefix of it, since the caller chooses the extent.
        for len in 0..=header.len() {
            let bytes = &header[..len];
            assert_eq!(header_crc16(bytes), low_16(bitwise_crc32(bytes)));
        }
    }

    /// Published CRC-32 check values, truncated as the `CRC16` field truncates them.
    #[test]
    fn header_crc16_matches_known_vectors() {
        // The empty input leaves the seed untouched, so the check value is zero.
        assert_eq!(header_crc16(&[]), 0);

        // `crc32.c`'s own documented vectors, and the payload of the fixture above.
        assert_eq!(header_crc16(b"a"), 0xbe43);
        assert_eq!(header_crc16(b"123456789"), 0x3926);
        assert_eq!(header_crc16(HELLO), 0xa686);

        // The full check values those come from, to make the truncation visible.
        assert_eq!(bitwise_crc32(b"123456789"), 0xcbf4_3926);
        assert_eq!(bitwise_crc32(HELLO), 0x3610_a686);
    }

    /// The value depends on every header byte, so a single-bit change in any of them is detected.
    #[test]
    fn header_crc16_detects_a_single_bit_change_in_any_header_byte() {
        let header = fixed_header(FLG_FHCRC, OS_UNKNOWN);
        let baseline = header_crc16(&header);

        for index in 0..header.len() {
            for bit in 0..8 {
                let mut mutated = header;
                mutated[index] ^= 1 << bit;

                assert_ne!(
                    header_crc16(&mutated),
                    baseline,
                    "flipping bit {bit} of header byte {index} must change the CRC16"
                );
            }
        }
    }

    /// A `CRC16` is written to the wire little-endian, like every other multi-byte field.
    ///
    /// `doc/rfc1952.txt` L266-L268 places the field immediately before the compressed blocks; the
    /// engine writes it there. This checks only that the value and the codec agree about byte order.
    #[test]
    fn header_crc16_encodes_little_endian() {
        let header = fixed_header(FLG_FHCRC, OS_HIGHEST_DEFINED);
        let crc16 = header_crc16(&header);

        let mut field = [0u8; U16_LEN];
        assert_eq!(write_u16_le(&mut field, crc16), Some(()));

        assert_eq!(u16_le(&field), Some(crc16));
        assert_eq!(field, crc16.to_le_bytes());

        // The two bytes are the low half of the full check value, in wire order.
        let full = bitwise_crc32(&header).to_le_bytes();
        assert_eq!(field, [full[0], full[1]]);
    }
}
