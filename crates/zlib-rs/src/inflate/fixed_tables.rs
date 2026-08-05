//! The fixed Huffman decode tables, transcribed verbatim from `inffixed.h`.
//!
//! `inffixed.h` L1-L2 carries the banner "table for decoding fixed codes / Generated
//! automatically by `makefixed()`", and L5-L8 warns that the file "should *not* be
//! used by applications", that it is part of the implementation of this library, and
//! that it is subject to change. Both statements carry over unchanged. Every value
//! below is a transcription of that machine-written header, and none of it is part of
//! the C ABI: a consumer reaches these tables only by decompressing a block that was
//! encoded with the fixed codes.
//!
//! # What the two tables are
//!
//! RFC 1951 §3.2.6 fixes one pair of Huffman codes that an encoder may use without
//! transmitting them -- literal/length codes of 7, 8 and 9 bits, and 32 distance
//! codes of 5 bits each. Because those code lengths are fixed by the specification,
//! the decode tables built from them are fixed too, so the reference implementation
//! ships them as data rather than rebuilding them for every stream: `inftrees.c`
//! L351 `#include`s this header, and `inflate_fixed` (`inftrees.c` L364-L372) then
//! points `state->lencode` at `lenfix` with `lenbits = 9` and `state->distcode` at
//! `distfix` with `distbits = 5`.
//!
//! [`lenfix`] is therefore a *complete* 9-bit root table and [`distfix`] a complete
//! 5-bit one. Neither contains a table link, because in each alphabet the longest
//! code is exactly as wide as the root and no second level is ever needed. Both
//! properties are asserted by the tests below, together with the entry counts that
//! RFC 1951 §3.2.6 implies.
//!
//! # Why these tables are transcribed and never computed
//!
//! `makefixed()` -- the generator at `inftrees.c` L374-L424 that wrote `inffixed.h`
//! -- does not print the table it built. Its `printf` at `inftrees.c` L405 reads
//!
//! ```text
//! printf("{%u,%u,%d}", (low & 127) == 99 ? 64 : state.lencode[low].op,
//!        state.lencode[low].bits, state.lencode[low].val);
//! ```
//!
//! substituting an `op` of 64 -- the plain invalid-code marker of `inftrees.h` L35 --
//! at every index where `low & 127 == 99`. Those four indices, 99, 227, 355 and 483,
//! hold the never-occurring symbols 286 and 287, whose real `op` values are the
//! `LEXT` sentinels 68 and 193. Every one of those three values is rejected by the
//! decoder, so the substitution is harmless -- but it does mean that the committed
//! header is **not** what `inflate_table` produces.
//!
//! Rebuilding [`lenfix`], whether at run time or in a `const` evaluation, would
//! therefore yield a table that differs from the one every existing zlib build
//! decodes with. The committed bytes are the contract: they are transcribed here, and
//! the substitution is pinned by the `makefixed_substituted_the_invalid_code_marker`
//! test below, so that no later change can quietly turn this module into a generated
//! one. `distfix` needs no such caveat -- `inftrees.c` L416 prints its `op`
//! unmodified.
//!
//! # No lazy initialisation
//!
//! These tables are `const` data, materialised by the compiler. `inftrees.c`
//! L313-L352 offers the alternative: a `BUILDFIXED` build that constructs them on
//! first use behind the `z_once_t` at `inftrees.c` L325 and which, as `inftrees.c`
//! L314-L319 warns, is not thread-safe when atomics are unavailable. This port
//! implements the default configuration only, so there is no `static mut`, no
//! `Once`, no `OnceLock`, no interior mutability and no `<stdatomic.h>` dependency to
//! inherit. Two consequences are worth knowing:
//!
//! * the facade reports `zlibCompileFlags` bit 12 (`BUILDFIXED`) as clear, which is
//!   the honest answer for a build that carries the committed tables;
//! * the library is about 2K bytes larger than a `BUILDFIXED` build would be, which
//!   is exactly the trade `inftrees.c` L359-L360 describes. 544 entries of four
//!   bytes is 2176 bytes, and that is the whole of the cost.
//!
//! # Naming and layout
//!
//! The two constants keep their C names, lowercase and unchanged, so that the
//! element-for-element comparison in
//! `crates/zlib-rs-differential/tests/table_equality.rs` reads as a direct diff
//! against the header. That is the sole reason each carries a narrowly scoped
//! `#[allow(non_upper_case_globals)]`.
//!
//! The header prints `lenfix` seven entries to a line and `distfix` six
//! (`inftrees.c` L404 and L415). That grouping is deliberately not reproduced:
//! `.rustfmt.toml` states the policy for exactly these tables -- they "are reflowed
//! like any other array literal" -- and `cargo fmt` lays a `[Code; 512]` out one
//! entry to a line. Fidelity is guaranteed mechanically instead of visually. The
//! transcription was extracted from `inffixed.h` by script, and the tests below
//! re-check the declared sizes, the endpoints, the first printed row of each table,
//! the substituted entries, the legal shape of every `op` and `bits` field, the
//! replication structure a root table must have, and the entry counts implied by
//! RFC 1951 §3.2.6.

use super::inftrees::{Code, FIXED_DISTBITS, FIXED_LENBITS};

/// Fixed literal/length decode table: `lenfix[512]`, transcribed from `inffixed.h`
/// L10-L85.
///
/// A complete root table for the fixed literal/length alphabet of RFC 1951 §3.2.6,
/// indexed by nine bits taken from the stream. Because `inflate` consumes bits
/// least-significant first while RFC 1951 §3.1.1 packs a Huffman code
/// most-significant first, the index is the code reversed -- which is why the table
/// reads as scattered rather than sorted. `inflate_fixed` installs it together with
/// `lenbits = 9` (`inftrees.c` L368-L369), so [`FIXED_LENBITS`] and this length are
/// two views of the same fact.
///
/// Four entries -- indices 99, 227, 355 and 483 -- carry the `op` of 64 that
/// `makefixed()` substituted at `inftrees.c` L405 rather than the value
/// `inflate_table` computed. See the module documentation: it is why this table is
/// transcribed and never generated.
// The identifier is deliberately the C name `lenfix` from `inffixed.h`; renaming it
// to SCREAMING_SNAKE_CASE would break the direct correspondence that the
// differential table-equality test relies on.
#[allow(non_upper_case_globals)]
pub const lenfix: [Code; 512] = [
    Code::new(96, 7, 0),
    Code::new(0, 8, 80),
    Code::new(0, 8, 16),
    Code::new(20, 8, 115),
    Code::new(18, 7, 31),
    Code::new(0, 8, 112),
    Code::new(0, 8, 48),
    Code::new(0, 9, 192),
    Code::new(16, 7, 10),
    Code::new(0, 8, 96),
    Code::new(0, 8, 32),
    Code::new(0, 9, 160),
    Code::new(0, 8, 0),
    Code::new(0, 8, 128),
    Code::new(0, 8, 64),
    Code::new(0, 9, 224),
    Code::new(16, 7, 6),
    Code::new(0, 8, 88),
    Code::new(0, 8, 24),
    Code::new(0, 9, 144),
    Code::new(19, 7, 59),
    Code::new(0, 8, 120),
    Code::new(0, 8, 56),
    Code::new(0, 9, 208),
    Code::new(17, 7, 17),
    Code::new(0, 8, 104),
    Code::new(0, 8, 40),
    Code::new(0, 9, 176),
    Code::new(0, 8, 8),
    Code::new(0, 8, 136),
    Code::new(0, 8, 72),
    Code::new(0, 9, 240),
    Code::new(16, 7, 4),
    Code::new(0, 8, 84),
    Code::new(0, 8, 20),
    Code::new(21, 8, 227),
    Code::new(19, 7, 43),
    Code::new(0, 8, 116),
    Code::new(0, 8, 52),
    Code::new(0, 9, 200),
    Code::new(17, 7, 13),
    Code::new(0, 8, 100),
    Code::new(0, 8, 36),
    Code::new(0, 9, 168),
    Code::new(0, 8, 4),
    Code::new(0, 8, 132),
    Code::new(0, 8, 68),
    Code::new(0, 9, 232),
    Code::new(16, 7, 8),
    Code::new(0, 8, 92),
    Code::new(0, 8, 28),
    Code::new(0, 9, 152),
    Code::new(20, 7, 83),
    Code::new(0, 8, 124),
    Code::new(0, 8, 60),
    Code::new(0, 9, 216),
    Code::new(18, 7, 23),
    Code::new(0, 8, 108),
    Code::new(0, 8, 44),
    Code::new(0, 9, 184),
    Code::new(0, 8, 12),
    Code::new(0, 8, 140),
    Code::new(0, 8, 76),
    Code::new(0, 9, 248),
    Code::new(16, 7, 3),
    Code::new(0, 8, 82),
    Code::new(0, 8, 18),
    Code::new(21, 8, 163),
    Code::new(19, 7, 35),
    Code::new(0, 8, 114),
    Code::new(0, 8, 50),
    Code::new(0, 9, 196),
    Code::new(17, 7, 11),
    Code::new(0, 8, 98),
    Code::new(0, 8, 34),
    Code::new(0, 9, 164),
    Code::new(0, 8, 2),
    Code::new(0, 8, 130),
    Code::new(0, 8, 66),
    Code::new(0, 9, 228),
    Code::new(16, 7, 7),
    Code::new(0, 8, 90),
    Code::new(0, 8, 26),
    Code::new(0, 9, 148),
    Code::new(20, 7, 67),
    Code::new(0, 8, 122),
    Code::new(0, 8, 58),
    Code::new(0, 9, 212),
    Code::new(18, 7, 19),
    Code::new(0, 8, 106),
    Code::new(0, 8, 42),
    Code::new(0, 9, 180),
    Code::new(0, 8, 10),
    Code::new(0, 8, 138),
    Code::new(0, 8, 74),
    Code::new(0, 9, 244),
    Code::new(16, 7, 5),
    Code::new(0, 8, 86),
    Code::new(0, 8, 22),
    Code::new(64, 8, 0),
    Code::new(19, 7, 51),
    Code::new(0, 8, 118),
    Code::new(0, 8, 54),
    Code::new(0, 9, 204),
    Code::new(17, 7, 15),
    Code::new(0, 8, 102),
    Code::new(0, 8, 38),
    Code::new(0, 9, 172),
    Code::new(0, 8, 6),
    Code::new(0, 8, 134),
    Code::new(0, 8, 70),
    Code::new(0, 9, 236),
    Code::new(16, 7, 9),
    Code::new(0, 8, 94),
    Code::new(0, 8, 30),
    Code::new(0, 9, 156),
    Code::new(20, 7, 99),
    Code::new(0, 8, 126),
    Code::new(0, 8, 62),
    Code::new(0, 9, 220),
    Code::new(18, 7, 27),
    Code::new(0, 8, 110),
    Code::new(0, 8, 46),
    Code::new(0, 9, 188),
    Code::new(0, 8, 14),
    Code::new(0, 8, 142),
    Code::new(0, 8, 78),
    Code::new(0, 9, 252),
    Code::new(96, 7, 0),
    Code::new(0, 8, 81),
    Code::new(0, 8, 17),
    Code::new(21, 8, 131),
    Code::new(18, 7, 31),
    Code::new(0, 8, 113),
    Code::new(0, 8, 49),
    Code::new(0, 9, 194),
    Code::new(16, 7, 10),
    Code::new(0, 8, 97),
    Code::new(0, 8, 33),
    Code::new(0, 9, 162),
    Code::new(0, 8, 1),
    Code::new(0, 8, 129),
    Code::new(0, 8, 65),
    Code::new(0, 9, 226),
    Code::new(16, 7, 6),
    Code::new(0, 8, 89),
    Code::new(0, 8, 25),
    Code::new(0, 9, 146),
    Code::new(19, 7, 59),
    Code::new(0, 8, 121),
    Code::new(0, 8, 57),
    Code::new(0, 9, 210),
    Code::new(17, 7, 17),
    Code::new(0, 8, 105),
    Code::new(0, 8, 41),
    Code::new(0, 9, 178),
    Code::new(0, 8, 9),
    Code::new(0, 8, 137),
    Code::new(0, 8, 73),
    Code::new(0, 9, 242),
    Code::new(16, 7, 4),
    Code::new(0, 8, 85),
    Code::new(0, 8, 21),
    Code::new(16, 8, 258),
    Code::new(19, 7, 43),
    Code::new(0, 8, 117),
    Code::new(0, 8, 53),
    Code::new(0, 9, 202),
    Code::new(17, 7, 13),
    Code::new(0, 8, 101),
    Code::new(0, 8, 37),
    Code::new(0, 9, 170),
    Code::new(0, 8, 5),
    Code::new(0, 8, 133),
    Code::new(0, 8, 69),
    Code::new(0, 9, 234),
    Code::new(16, 7, 8),
    Code::new(0, 8, 93),
    Code::new(0, 8, 29),
    Code::new(0, 9, 154),
    Code::new(20, 7, 83),
    Code::new(0, 8, 125),
    Code::new(0, 8, 61),
    Code::new(0, 9, 218),
    Code::new(18, 7, 23),
    Code::new(0, 8, 109),
    Code::new(0, 8, 45),
    Code::new(0, 9, 186),
    Code::new(0, 8, 13),
    Code::new(0, 8, 141),
    Code::new(0, 8, 77),
    Code::new(0, 9, 250),
    Code::new(16, 7, 3),
    Code::new(0, 8, 83),
    Code::new(0, 8, 19),
    Code::new(21, 8, 195),
    Code::new(19, 7, 35),
    Code::new(0, 8, 115),
    Code::new(0, 8, 51),
    Code::new(0, 9, 198),
    Code::new(17, 7, 11),
    Code::new(0, 8, 99),
    Code::new(0, 8, 35),
    Code::new(0, 9, 166),
    Code::new(0, 8, 3),
    Code::new(0, 8, 131),
    Code::new(0, 8, 67),
    Code::new(0, 9, 230),
    Code::new(16, 7, 7),
    Code::new(0, 8, 91),
    Code::new(0, 8, 27),
    Code::new(0, 9, 150),
    Code::new(20, 7, 67),
    Code::new(0, 8, 123),
    Code::new(0, 8, 59),
    Code::new(0, 9, 214),
    Code::new(18, 7, 19),
    Code::new(0, 8, 107),
    Code::new(0, 8, 43),
    Code::new(0, 9, 182),
    Code::new(0, 8, 11),
    Code::new(0, 8, 139),
    Code::new(0, 8, 75),
    Code::new(0, 9, 246),
    Code::new(16, 7, 5),
    Code::new(0, 8, 87),
    Code::new(0, 8, 23),
    Code::new(64, 8, 0),
    Code::new(19, 7, 51),
    Code::new(0, 8, 119),
    Code::new(0, 8, 55),
    Code::new(0, 9, 206),
    Code::new(17, 7, 15),
    Code::new(0, 8, 103),
    Code::new(0, 8, 39),
    Code::new(0, 9, 174),
    Code::new(0, 8, 7),
    Code::new(0, 8, 135),
    Code::new(0, 8, 71),
    Code::new(0, 9, 238),
    Code::new(16, 7, 9),
    Code::new(0, 8, 95),
    Code::new(0, 8, 31),
    Code::new(0, 9, 158),
    Code::new(20, 7, 99),
    Code::new(0, 8, 127),
    Code::new(0, 8, 63),
    Code::new(0, 9, 222),
    Code::new(18, 7, 27),
    Code::new(0, 8, 111),
    Code::new(0, 8, 47),
    Code::new(0, 9, 190),
    Code::new(0, 8, 15),
    Code::new(0, 8, 143),
    Code::new(0, 8, 79),
    Code::new(0, 9, 254),
    Code::new(96, 7, 0),
    Code::new(0, 8, 80),
    Code::new(0, 8, 16),
    Code::new(20, 8, 115),
    Code::new(18, 7, 31),
    Code::new(0, 8, 112),
    Code::new(0, 8, 48),
    Code::new(0, 9, 193),
    Code::new(16, 7, 10),
    Code::new(0, 8, 96),
    Code::new(0, 8, 32),
    Code::new(0, 9, 161),
    Code::new(0, 8, 0),
    Code::new(0, 8, 128),
    Code::new(0, 8, 64),
    Code::new(0, 9, 225),
    Code::new(16, 7, 6),
    Code::new(0, 8, 88),
    Code::new(0, 8, 24),
    Code::new(0, 9, 145),
    Code::new(19, 7, 59),
    Code::new(0, 8, 120),
    Code::new(0, 8, 56),
    Code::new(0, 9, 209),
    Code::new(17, 7, 17),
    Code::new(0, 8, 104),
    Code::new(0, 8, 40),
    Code::new(0, 9, 177),
    Code::new(0, 8, 8),
    Code::new(0, 8, 136),
    Code::new(0, 8, 72),
    Code::new(0, 9, 241),
    Code::new(16, 7, 4),
    Code::new(0, 8, 84),
    Code::new(0, 8, 20),
    Code::new(21, 8, 227),
    Code::new(19, 7, 43),
    Code::new(0, 8, 116),
    Code::new(0, 8, 52),
    Code::new(0, 9, 201),
    Code::new(17, 7, 13),
    Code::new(0, 8, 100),
    Code::new(0, 8, 36),
    Code::new(0, 9, 169),
    Code::new(0, 8, 4),
    Code::new(0, 8, 132),
    Code::new(0, 8, 68),
    Code::new(0, 9, 233),
    Code::new(16, 7, 8),
    Code::new(0, 8, 92),
    Code::new(0, 8, 28),
    Code::new(0, 9, 153),
    Code::new(20, 7, 83),
    Code::new(0, 8, 124),
    Code::new(0, 8, 60),
    Code::new(0, 9, 217),
    Code::new(18, 7, 23),
    Code::new(0, 8, 108),
    Code::new(0, 8, 44),
    Code::new(0, 9, 185),
    Code::new(0, 8, 12),
    Code::new(0, 8, 140),
    Code::new(0, 8, 76),
    Code::new(0, 9, 249),
    Code::new(16, 7, 3),
    Code::new(0, 8, 82),
    Code::new(0, 8, 18),
    Code::new(21, 8, 163),
    Code::new(19, 7, 35),
    Code::new(0, 8, 114),
    Code::new(0, 8, 50),
    Code::new(0, 9, 197),
    Code::new(17, 7, 11),
    Code::new(0, 8, 98),
    Code::new(0, 8, 34),
    Code::new(0, 9, 165),
    Code::new(0, 8, 2),
    Code::new(0, 8, 130),
    Code::new(0, 8, 66),
    Code::new(0, 9, 229),
    Code::new(16, 7, 7),
    Code::new(0, 8, 90),
    Code::new(0, 8, 26),
    Code::new(0, 9, 149),
    Code::new(20, 7, 67),
    Code::new(0, 8, 122),
    Code::new(0, 8, 58),
    Code::new(0, 9, 213),
    Code::new(18, 7, 19),
    Code::new(0, 8, 106),
    Code::new(0, 8, 42),
    Code::new(0, 9, 181),
    Code::new(0, 8, 10),
    Code::new(0, 8, 138),
    Code::new(0, 8, 74),
    Code::new(0, 9, 245),
    Code::new(16, 7, 5),
    Code::new(0, 8, 86),
    Code::new(0, 8, 22),
    Code::new(64, 8, 0),
    Code::new(19, 7, 51),
    Code::new(0, 8, 118),
    Code::new(0, 8, 54),
    Code::new(0, 9, 205),
    Code::new(17, 7, 15),
    Code::new(0, 8, 102),
    Code::new(0, 8, 38),
    Code::new(0, 9, 173),
    Code::new(0, 8, 6),
    Code::new(0, 8, 134),
    Code::new(0, 8, 70),
    Code::new(0, 9, 237),
    Code::new(16, 7, 9),
    Code::new(0, 8, 94),
    Code::new(0, 8, 30),
    Code::new(0, 9, 157),
    Code::new(20, 7, 99),
    Code::new(0, 8, 126),
    Code::new(0, 8, 62),
    Code::new(0, 9, 221),
    Code::new(18, 7, 27),
    Code::new(0, 8, 110),
    Code::new(0, 8, 46),
    Code::new(0, 9, 189),
    Code::new(0, 8, 14),
    Code::new(0, 8, 142),
    Code::new(0, 8, 78),
    Code::new(0, 9, 253),
    Code::new(96, 7, 0),
    Code::new(0, 8, 81),
    Code::new(0, 8, 17),
    Code::new(21, 8, 131),
    Code::new(18, 7, 31),
    Code::new(0, 8, 113),
    Code::new(0, 8, 49),
    Code::new(0, 9, 195),
    Code::new(16, 7, 10),
    Code::new(0, 8, 97),
    Code::new(0, 8, 33),
    Code::new(0, 9, 163),
    Code::new(0, 8, 1),
    Code::new(0, 8, 129),
    Code::new(0, 8, 65),
    Code::new(0, 9, 227),
    Code::new(16, 7, 6),
    Code::new(0, 8, 89),
    Code::new(0, 8, 25),
    Code::new(0, 9, 147),
    Code::new(19, 7, 59),
    Code::new(0, 8, 121),
    Code::new(0, 8, 57),
    Code::new(0, 9, 211),
    Code::new(17, 7, 17),
    Code::new(0, 8, 105),
    Code::new(0, 8, 41),
    Code::new(0, 9, 179),
    Code::new(0, 8, 9),
    Code::new(0, 8, 137),
    Code::new(0, 8, 73),
    Code::new(0, 9, 243),
    Code::new(16, 7, 4),
    Code::new(0, 8, 85),
    Code::new(0, 8, 21),
    Code::new(16, 8, 258),
    Code::new(19, 7, 43),
    Code::new(0, 8, 117),
    Code::new(0, 8, 53),
    Code::new(0, 9, 203),
    Code::new(17, 7, 13),
    Code::new(0, 8, 101),
    Code::new(0, 8, 37),
    Code::new(0, 9, 171),
    Code::new(0, 8, 5),
    Code::new(0, 8, 133),
    Code::new(0, 8, 69),
    Code::new(0, 9, 235),
    Code::new(16, 7, 8),
    Code::new(0, 8, 93),
    Code::new(0, 8, 29),
    Code::new(0, 9, 155),
    Code::new(20, 7, 83),
    Code::new(0, 8, 125),
    Code::new(0, 8, 61),
    Code::new(0, 9, 219),
    Code::new(18, 7, 23),
    Code::new(0, 8, 109),
    Code::new(0, 8, 45),
    Code::new(0, 9, 187),
    Code::new(0, 8, 13),
    Code::new(0, 8, 141),
    Code::new(0, 8, 77),
    Code::new(0, 9, 251),
    Code::new(16, 7, 3),
    Code::new(0, 8, 83),
    Code::new(0, 8, 19),
    Code::new(21, 8, 195),
    Code::new(19, 7, 35),
    Code::new(0, 8, 115),
    Code::new(0, 8, 51),
    Code::new(0, 9, 199),
    Code::new(17, 7, 11),
    Code::new(0, 8, 99),
    Code::new(0, 8, 35),
    Code::new(0, 9, 167),
    Code::new(0, 8, 3),
    Code::new(0, 8, 131),
    Code::new(0, 8, 67),
    Code::new(0, 9, 231),
    Code::new(16, 7, 7),
    Code::new(0, 8, 91),
    Code::new(0, 8, 27),
    Code::new(0, 9, 151),
    Code::new(20, 7, 67),
    Code::new(0, 8, 123),
    Code::new(0, 8, 59),
    Code::new(0, 9, 215),
    Code::new(18, 7, 19),
    Code::new(0, 8, 107),
    Code::new(0, 8, 43),
    Code::new(0, 9, 183),
    Code::new(0, 8, 11),
    Code::new(0, 8, 139),
    Code::new(0, 8, 75),
    Code::new(0, 9, 247),
    Code::new(16, 7, 5),
    Code::new(0, 8, 87),
    Code::new(0, 8, 23),
    Code::new(64, 8, 0),
    Code::new(19, 7, 51),
    Code::new(0, 8, 119),
    Code::new(0, 8, 55),
    Code::new(0, 9, 207),
    Code::new(17, 7, 15),
    Code::new(0, 8, 103),
    Code::new(0, 8, 39),
    Code::new(0, 9, 175),
    Code::new(0, 8, 7),
    Code::new(0, 8, 135),
    Code::new(0, 8, 71),
    Code::new(0, 9, 239),
    Code::new(16, 7, 9),
    Code::new(0, 8, 95),
    Code::new(0, 8, 31),
    Code::new(0, 9, 159),
    Code::new(20, 7, 99),
    Code::new(0, 8, 127),
    Code::new(0, 8, 63),
    Code::new(0, 9, 223),
    Code::new(18, 7, 27),
    Code::new(0, 8, 111),
    Code::new(0, 8, 47),
    Code::new(0, 9, 191),
    Code::new(0, 8, 15),
    Code::new(0, 8, 143),
    Code::new(0, 8, 79),
    Code::new(0, 9, 255),
];

/// Fixed distance decode table: `distfix[32]`, transcribed from `inffixed.h`
/// L87-L94.
///
/// A complete root table for the 32 five-bit distance codes of RFC 1951 §3.2.6,
/// indexed by five reversed bits exactly as [`lenfix`] is indexed by nine.
/// `inflate_fixed` installs it with `distbits = 5` (`inftrees.c` L370-L371), which
/// is [`FIXED_DISTBITS`].
///
/// Thirty of the entries carry a distance base in `val` and the count of extra bits
/// to read in the low nibble of `op`. The remaining two -- the codes for symbols 30
/// and 31, which RFC 1951 §3.2.5 leaves undefined -- carry the invalid-code marker
/// 64, written by `inflate_table` itself at `inftrees.c` L127 from the `DEXT`
/// sentinel. Unlike [`lenfix`], nothing here was rewritten by the generator:
/// `inftrees.c` L416 prints `state.distcode[low].op` verbatim.
// The identifier is deliberately the C name `distfix` from `inffixed.h`; see the note
// on `lenfix` above.
#[allow(non_upper_case_globals)]
pub const distfix: [Code; 32] = [
    Code::new(16, 5, 1),
    Code::new(23, 5, 257),
    Code::new(19, 5, 17),
    Code::new(27, 5, 4097),
    Code::new(17, 5, 5),
    Code::new(25, 5, 1025),
    Code::new(21, 5, 65),
    Code::new(29, 5, 16385),
    Code::new(16, 5, 3),
    Code::new(24, 5, 513),
    Code::new(20, 5, 33),
    Code::new(28, 5, 8193),
    Code::new(18, 5, 9),
    Code::new(26, 5, 2049),
    Code::new(22, 5, 129),
    Code::new(64, 5, 0),
    Code::new(16, 5, 2),
    Code::new(23, 5, 385),
    Code::new(19, 5, 25),
    Code::new(27, 5, 6145),
    Code::new(17, 5, 7),
    Code::new(25, 5, 1537),
    Code::new(21, 5, 97),
    Code::new(29, 5, 24577),
    Code::new(16, 5, 4),
    Code::new(24, 5, 769),
    Code::new(20, 5, 49),
    Code::new(28, 5, 12289),
    Code::new(18, 5, 13),
    Code::new(26, 5, 3073),
    Code::new(22, 5, 193),
    Code::new(64, 5, 0),
];

// The declared size of each table is the width of the root index `inflate_fixed`
// installs alongside it (`inftrees.c` L369 and L371), so `1 << bits` is not a
// coincidence but the definition of a complete root table. Asserting it at compile
// time ties the transcription to the shift widths the decoder actually uses: a table
// that gained or lost an entry, or a root width that drifted, fails the build instead
// of mis-decoding a fixed-code block at run time.
const _: () = assert!(lenfix.len() == 1 << FIXED_LENBITS);
const _: () = assert!(distfix.len() == 1 << FIXED_DISTBITS);

#[cfg(test)]
mod tests {
    use super::{distfix, lenfix, Code, FIXED_DISTBITS, FIXED_LENBITS};

    /// The first row `makefixed()` prints for `lenfix` -- seven entries to a line
    /// (`inftrees.c` L404) -- quoted from `inffixed.h` L11 as `(op, bits, val)`.
    const LENFIX_FIRST_ROW: [(u8, u8, u16); 7] = [
        (96, 7, 0),
        (0, 8, 80),
        (0, 8, 16),
        (20, 8, 115),
        (18, 7, 31),
        (0, 8, 112),
        (0, 8, 48),
    ];

    /// The first row `makefixed()` prints for `distfix` -- six entries to a line
    /// (`inftrees.c` L415) -- quoted from `inffixed.h` L88.
    const DISTFIX_FIRST_ROW: [(u8, u8, u16); 6] = [
        (16, 5, 1),
        (23, 5, 257),
        (19, 5, 17),
        (27, 5, 4097),
        (17, 5, 5),
        (25, 5, 1025),
    ];

    /// The indices `makefixed()` rewrites: those satisfying `low & 127 == 99`
    /// (`inftrees.c` L405).
    const SUBSTITUTED: [usize; 4] = [99, 227, 355, 483];

    /// `MAXBITS` from `inftrees.c` L23 -- the widest Huffman code DEFLATE permits,
    /// and therefore the upper bound on any entry's `bits`.
    const MAXBITS: u8 = 15;

    /// Reverses the low five bits of `value`, mapping a distance symbol to the index
    /// its code occupies in [`distfix`].
    ///
    /// All 32 distance codes are five bits wide, so canonical assignment gives
    /// symbol `s` the code `s` (RFC 1951 §3.2.2). RFC 1951 §3.1.1 then packs that
    /// code most-significant bit first while the decoder consumes bits
    /// least-significant first, so the table is indexed by the reversal -- the same
    /// relationship `inflate_table` builds incrementally at `inftrees.c` L245-L251.
    fn reverse_five_bits(value: usize) -> usize {
        let mut source = value;
        let mut reversed = 0;
        for _ in 0..5 {
            reversed = (reversed << 1) | (source & 1);
            source >>= 1;
        }
        reversed
    }

    #[test]
    fn table_lengths_are_the_sizes_declared_in_the_header() {
        assert_eq!(lenfix.len(), 512, "`inffixed.h` L10 declares lenfix[512]");
        assert_eq!(distfix.len(), 32, "`inffixed.h` L87 declares distfix[32]");

        // Restated at run time as well as at compile time, so that a failure names
        // the mismatch rather than only refusing to build.
        assert_eq!(lenfix.len(), 1 << FIXED_LENBITS);
        assert_eq!(distfix.len(), 1 << FIXED_DISTBITS);
    }

    #[test]
    fn endpoints_match_the_committed_header() {
        assert_eq!(lenfix[0], Code::new(96, 7, 0), "`inffixed.h` L11");
        assert_eq!(lenfix[511], Code::new(0, 9, 255), "`inffixed.h` L84");
        assert_eq!(distfix[0], Code::new(16, 5, 1), "`inffixed.h` L88");
        assert_eq!(distfix[31], Code::new(64, 5, 0), "`inffixed.h` L93");
    }

    #[test]
    fn first_printed_row_of_each_table_matches_the_header() {
        for (index, &(op, bits, val)) in LENFIX_FIRST_ROW.iter().enumerate() {
            assert_eq!(lenfix[index], Code::new(op, bits, val), "lenfix[{index}]");
        }
        for (index, &(op, bits, val)) in DISTFIX_FIRST_ROW.iter().enumerate() {
            assert_eq!(distfix[index], Code::new(op, bits, val), "distfix[{index}]");
        }
    }

    #[test]
    fn makefixed_substituted_the_invalid_code_marker() {
        // `makefixed()` does not print the table it built: the printf at
        // `inftrees.c` L405 writes an `op` of 64 wherever `(low & 127) == 99`, in
        // place of the `LEXT` sentinels 68 and 193 that `inflate_table` had stored
        // for the never-occurring symbols 286 and 287. This test is what makes the
        // module's "transcribe, never compute" rule enforceable: a version of
        // `lenfix` rebuilt by `inflate_table` would fail it.
        for index in SUBSTITUTED {
            assert_eq!(
                lenfix[index],
                Code::new(64, 8, 0),
                "`inffixed.h` must carry the substituted entry at {index}"
            );
            assert!(lenfix[index].is_invalid(), "at index {index}");
        }

        // The substituted set is exactly those indices -- no more, no fewer.
        for (index, entry) in lenfix.iter().enumerate() {
            assert_eq!(
                entry.op == 64,
                index & 127 == 99,
                "only `low & 127 == 99` may carry the substituted op, at {index}"
            );
        }
        assert_eq!(
            lenfix.iter().filter(|entry| entry.op == 64).count(),
            SUBSTITUTED.len()
        );
    }

    #[test]
    fn every_entry_has_a_legal_op_and_width() {
        // The five legal shapes are the table at `inftrees.h` L30-L36: a literal
        // (op 0), a table link (a non-zero low nibble alone), a length or distance
        // (op & 16, with the extra-bit count in the low nibble), end of block (96)
        // and an invalid code (64). A transposed column -- `op` and `bits` swapped,
        // say -- lands outside that set almost immediately, which is what makes this
        // sweep worth more than the endpoint checks alone.
        for (index, entry) in lenfix.iter().chain(distfix.iter()).enumerate() {
            assert!(
                (1..=MAXBITS).contains(&entry.bits),
                "bits {} out of range at flat index {index}",
                entry.bits
            );
            assert!(
                entry.is_literal()
                    || entry.is_length_or_distance()
                    || entry.is_end_of_block()
                    || entry.is_invalid(),
                "op {} at flat index {index} is not a shape from `inftrees.h` L30-L36",
                entry.op
            );
            assert!(
                !entry.is_table_link(),
                "a complete root table has no second level, at flat index {index}"
            );
        }

        // The fixed literal/length codes are 7, 8 or 9 bits (RFC 1951 §3.2.6) and
        // the table's root is 9, so no entry accounts for any other width. Lengths
        // need at most 5 extra bits, and no `op` beyond the ones the header
        // actually holds may appear.
        for (index, entry) in lenfix.iter().enumerate() {
            assert!(matches!(entry.bits, 7..=9), "lenfix[{index}]");
            assert!(matches!(entry.op, 0 | 16..=21 | 64 | 96), "lenfix[{index}]");
            if entry.is_length_or_distance() {
                assert!(entry.extra_bits() <= 5, "lenfix[{index}]");
                assert!((3..=258).contains(&entry.val), "lenfix[{index}]");
            } else if entry.is_literal() {
                assert!(entry.val <= 255, "lenfix[{index}]");
            } else {
                assert_eq!(entry.val, 0, "lenfix[{index}]");
            }
        }

        // Every distance code is five bits wide, needs at most 13 extra bits, and
        // carries a base in `1..=24577` (RFC 1951 §3.2.5).
        for (index, entry) in distfix.iter().enumerate() {
            assert_eq!(entry.bits, 5, "distfix[{index}]");
            assert!(matches!(entry.op, 16..=29 | 64), "distfix[{index}]");
            if entry.is_length_or_distance() {
                assert!(entry.extra_bits() <= 13, "distfix[{index}]");
                assert!((1..=24577).contains(&entry.val), "distfix[{index}]");
            } else {
                assert_eq!(*entry, Code::new(64, 5, 0), "distfix[{index}]");
            }
        }
    }

    #[test]
    fn entries_are_replicated_across_the_root_index() {
        // A code of `bits` bits occupies every index whose low `bits` bits match it:
        // that is the `incr`/`fill` loop at `inftrees.c` L243-L252, and it is what
        // lets the decoder index the root with a fixed nine or five bits and drop
        // only `bits` of them afterwards. So an entry must equal the entry at its
        // own index masked to its own width -- a single relation that ties all 544
        // entries together and catches a transcription that shifted or duplicated a
        // row.
        for table in [lenfix.as_slice(), distfix.as_slice()] {
            for (index, entry) in table.iter().enumerate() {
                let mask = (1usize << entry.bits) - 1;
                assert_eq!(
                    *entry,
                    table[index & mask],
                    "entry at {index} is not the entry at {}",
                    index & mask
                );
            }
        }
    }

    #[test]
    fn entry_counts_match_rfc_1951_section_3_2_6() {
        // RFC 1951 §3.2.6 gives the fixed literal/length alphabet 24 codes of seven
        // bits (symbols 256-279), 152 of eight (0-143 and 280-287) and 112 of nine
        // (144-255). A nine-bit root replicates a code of `bits` bits `1 << (9 -
        // bits)` times, so the widths must appear 96, 304 and 112 times.
        assert_eq!(
            lenfix.iter().filter(|entry| entry.bits == 7).count(),
            24 * 4
        );
        assert_eq!(
            lenfix.iter().filter(|entry| entry.bits == 8).count(),
            152 * 2
        );
        assert_eq!(lenfix.iter().filter(|entry| entry.bits == 9).count(), 112);

        // By shape: symbol 256 is end of block, a seven-bit code, so four entries.
        // Symbols 286 and 287 are eight-bit codes and never occur, so four entries
        // carry the invalid marker. The 256 literals give 144 * 2 + 112 = 400
        // entries and the 29 length codes 23 * 4 + 6 * 2 = 104, which accounts for
        // all 512.
        let end_of_block = lenfix
            .iter()
            .filter(|entry| entry.is_end_of_block())
            .count();
        let invalid = lenfix.iter().filter(|entry| entry.is_invalid()).count();
        let literals = lenfix.iter().filter(|entry| entry.is_literal()).count();
        let lengths = lenfix
            .iter()
            .filter(|entry| entry.is_length_or_distance())
            .count();
        assert_eq!(end_of_block, 4);
        assert_eq!(invalid, 4);
        assert_eq!(literals, 400);
        assert_eq!(lengths, 104);
        assert_eq!(end_of_block + invalid + literals + lengths, lenfix.len());

        // The distance alphabet is 32 five-bit codes, so each occupies exactly one
        // entry of a five-bit root; symbols 30 and 31 are the undefined ones.
        assert_eq!(
            distfix
                .iter()
                .filter(|entry| entry.is_length_or_distance())
                .count(),
            30
        );
        assert_eq!(distfix.iter().filter(|entry| entry.is_invalid()).count(), 2);
    }

    #[test]
    fn literal_entries_cover_every_byte_value() {
        // The literal codes are eight bits for 0-143 and nine bits for 144-255
        // (RFC 1951 §3.2.6), so in a nine-bit root every byte value appears -- twice
        // for the shorter codes, once for the longer. Nothing short of a
        // value-for-value comparison with the header proves as much about the `val`
        // column as this does.
        let mut occurrences = [0_u8; 256];
        for entry in &lenfix {
            if entry.is_literal() {
                occurrences[usize::from(entry.val)] += 1;
            }
        }
        for (value, &count) in occurrences.iter().enumerate() {
            let expected = if value < 144 { 2 } else { 1 };
            assert_eq!(count, expected, "literal {value}");
        }
    }

    #[test]
    fn distance_bases_and_extra_bits_follow_the_symbol_order() {
        // Read through the code reversal, `distfix` must reproduce RFC 1951 §3.2.5
        // exactly: distance symbol `s` carries `(s / 2) - 1` extra bits once `s` is
        // at least 4 and none below that, so its `op` is `16 + extra`, and the bases
        // increase strictly with the symbol from 1 up to 24577. Checking the table
        // in symbol order rather than index order is what makes a swapped pair of
        // rows visible.
        let mut previous_base = 0;
        for symbol in 0..30_usize {
            let entry = distfix[reverse_five_bits(symbol)];
            let extra = if symbol < 4 {
                0
            } else {
                u8::try_from(symbol / 2).unwrap() - 1
            };
            assert_eq!(entry.op, 16 + extra, "distance symbol {symbol}");
            assert!(
                entry.val > previous_base,
                "distance symbol {symbol}: base {} does not exceed {previous_base}",
                entry.val
            );
            previous_base = entry.val;
        }
        assert_eq!(
            distfix[reverse_five_bits(0)].val,
            1,
            "the shortest distance"
        );
        assert_eq!(previous_base, 24577, "the longest distance base");

        // RFC 1951 §3.2.5 defines no codes 30 and 31, and `inflate_table` writes the
        // invalid marker from the `DEXT` sentinel for them (`inftrees.c` L127).
        for symbol in 30..32_usize {
            assert_eq!(
                distfix[reverse_five_bits(symbol)],
                Code::new(64, 5, 0),
                "distance symbol {symbol}"
            );
        }
    }
}
