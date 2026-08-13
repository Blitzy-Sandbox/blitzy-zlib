#!/usr/bin/env python3
"""Generate -- or verify -- the committed per-target fuzz seed corpus.

# Why the seeds are generated rather than collected

Every target in `fuzz/fuzz_targets/` takes a **typed** record, not raw bytes:
`InflateInput`, `BackInput`, `FuzzInput`, `GzRoundTrip`, `ChecksumInput`. `arbitrary`
decodes that record from the input file, and it is infallible by design -- a file that is
not a sensible encoding of the record does not fail, it decodes to a *useless* record.
`windowBits` lands outside the legal space, or the payload comes out empty, and the
execution stops in the first refusal.

Seeding every target from the same directory of **uncompressed** fixtures did exactly that.
`crates/zlib-rs-differential/corpus/minimal/` holds excellent DEFLATE inputs and, read as
typed records, near-useless ones. A decoder target seeded that way starts with a corpus of
immediate error paths, and nothing in CI could tell the difference between a row that was
decoding real streams and a row that was not.

So the seeds are *encoded*, here, to `arbitrary` 1.4.2's own rules, and the encoding is
verified from the other side: `.github/workflows/rust.yml` runs each committed seed through
its target once with `ZLIB_RS_FUZZ_REPORT=1` and requires the productive `reached=` value to
appear. Neither half is trusted on its own.

# The encoding, which is not obvious

Taken from `arbitrary` 1.4.2's source, not from its documentation:

* **Integers** -- `fill_buffer` then `from_le_bytes`, from the FRONT of the buffer. `u8` one
  byte, `u16` two, `u32`/`i32` four. Missing bytes are zero-filled, not an error.
* **`bool`** -- one byte; the value is `byte & 1 == 1`.
* **`[T; N]`** -- N elements back to back, no length prefix.
* **`Vec<u8>` in a non-final field** -- `arbitrary_iter`, which reads a `bool` BEFORE EVERY
  ELEMENT. So `n` bytes cost `2n + 1`: `(0x01, byte)` per element and one even byte to stop.
* **`Vec<u8>` in the FINAL field** -- `arbitrary_take_rest`, and this is the trap:
  `ArbitraryTakeRestIter` reads that same continuation `bool` before every element. It does
  *not* take the remaining bytes verbatim. `n` bytes cost `2n`.
* **`&'a [u8]` in the FINAL field** -- `arbitrary_take_rest` really is `u.take_rest()`, so
  the payload is the remaining bytes verbatim, one for one.
* **A derived `enum`** -- a `u32` from the front; the variant is `(u64::from(x) * count) >> 32`.
* **A derived `struct`** -- fields in declaration order, `arbitrary` for all but the last and
  `arbitrary_take_rest` for the last.
* `libfuzzer-sys` refuses an input shorter than `size_hint().0` before decoding, so every
  seed must be at least as long as the record's fixed part.

The `Vec` rules are why an uncompressed fixture cannot accidentally be a good seed: read as
a record, every second byte of it is a continuation flag.

# Field order is checked, not assumed

Each encoder declares the field names it is encoding, in order, and `check_layout` reads the
`struct` out of the target's source and requires the two lists to match exactly. Reordering
a record's fields silently invalidates every seed for that target; this turns that into a
loud failure here, and `--check` turns it into a red CI job.

# Provenance of the compressed payloads

The zlib, gzip and raw DEFLATE streams embedded in the decoder seeds are produced by the
**host's zlib** through Python's `zlib` module, at the level and container named in each
file name. That is the right tool for the job and the claim is deliberately no stronger than
it needs to be: what a decoder seed has to be is a *conformant* stream, and that it is one
is proven by the CI step that shows the port decoding it to `Z_STREAM_END` -- not by where
it came from. Byte-identity against the in-tree C oracle is a different property, it is not
needed here, and it is already gated exhaustively by
`crates/zlib-rs-differential/tests/byte_identical.rs`.

# Usage

    python3 fuzz/seeds/generate.py            # write the seeds
    python3 fuzz/seeds/generate.py --check    # verify the committed seeds, write nothing

`--check` is what CI runs. It fails if a seed is missing, differs, or is not accounted for by
this script -- so a stray file in `fuzz/seeds/` is a failure too.
"""

from __future__ import annotations

import argparse
import re
import struct
import sys
import zlib
from dataclasses import dataclass
from pathlib import Path

# ---------------------------------------------------------------------------
#  arbitrary 1.4.2 encoding primitives
# ---------------------------------------------------------------------------


def u8(value: int) -> bytes:
    """One byte."""
    return struct.pack("<B", value & 0xFF)


def u16(value: int) -> bytes:
    """Two little-endian bytes."""
    return struct.pack("<H", value & 0xFFFF)


def u32(value: int) -> bytes:
    """Four little-endian bytes."""
    return struct.pack("<I", value & 0xFFFF_FFFF)


def i8(value: int) -> bytes:
    """One byte, two's complement."""
    return struct.pack("<b", value)


def i16(value: int) -> bytes:
    """Two little-endian bytes, two's complement."""
    return struct.pack("<h", value)


def i32(value: int) -> bytes:
    """Four little-endian bytes, two's complement."""
    return struct.pack("<i", value)


def boolean(value: bool) -> bytes:
    """One byte, odd for true."""
    return u8(1 if value else 0)


def array_u8(values: bytes | list[int], length: int) -> bytes:
    """`[u8; length]`, zero-padded if `values` is short."""
    body = bytes(values)[:length]
    return body + bytes(length - len(body))


def array_i16(values: list[int], length: int) -> bytes:
    """`[i16; length]`, zero-padded."""
    padded = list(values) + [0] * (length - len(values))
    return b"".join(i16(v) for v in padded[:length])


def array_i32(values: list[int], length: int) -> bytes:
    """`[i32; length]`, zero-padded."""
    padded = list(values) + [0] * (length - len(values))
    return b"".join(i32(v) for v in padded[:length])


def vec_u8(data: bytes) -> bytes:
    """`Vec<u8>` in a NON-final field: `(0x01, byte)` per element, then one even byte."""
    return b"".join(u8(1) + u8(b) for b in data) + u8(0)


def vec_u8_take_rest(data: bytes) -> bytes:
    """`Vec<u8>` in the FINAL field: `(0x01, byte)` per element, terminated by end of data.

    The continuation byte is required here too -- `ArbitraryTakeRestIter` reads one before
    every element -- which is the single most surprising rule in the whole encoding and the
    reason a raw compressed stream appended to a header decodes to roughly half of itself.
    """
    return b"".join(u8(1) + u8(b) for b in data)


def slice_take_rest(data: bytes) -> bytes:
    """`&'a [u8]` in the FINAL field: the bytes verbatim."""
    return bytes(data)


def enum_variant(index: int, count: int) -> bytes:
    """The `u32` that selects variant `index` of a derived enum with `count` variants.

    The derive computes `(u64::from(x) * count) >> 32`, so the lowest `x` selecting `index`
    is `ceil(index * 2**32 / count)`.
    """
    if not 0 <= index < count:
        raise ValueError(f"variant {index} out of range for {count} variants")
    return u32(-(-index * (1 << 32) // count))


# ---------------------------------------------------------------------------
#  Payloads
# ---------------------------------------------------------------------------

#: The exact string `test/example.c` L35 compresses, NUL included.
HELLO = b"hello, hello!\x00"

#: The preset dictionary `test/example.c` L36 uses.
DICTIONARY = b"hello"

#: Long enough to make the decoder emit both a literal run and a copy, and to cross the
#: 32-byte prefix the checksum target treats as its short/long boundary.
PROSE = (
    b"the quick brown fox jumps over the lazy dog; "
    b"the quick brown fox jumps over the lazy dog again."
)


def zlib_stream(data: bytes, level: int = 6) -> bytes:
    """An RFC 1950 zlib stream."""
    return zlib.compress(data, level)


def raw_stream(data: bytes, level: int = 6) -> bytes:
    """A bare RFC 1951 DEFLATE stream, no wrapper."""
    engine = zlib.compressobj(level, zlib.DEFLATED, -15)
    return engine.compress(data) + engine.flush()


def gzip_stream(data: bytes, level: int = 6) -> bytes:
    """An RFC 1952 gzip stream with only the mandatory fields."""
    engine = zlib.compressobj(level, zlib.DEFLATED, 31)
    return engine.compress(data) + engine.flush()


def gzip_stream_with_fields(data: bytes) -> bytes:
    """A gzip stream carrying FEXTRA, FNAME, FCOMMENT and FHCRC.

    Hand-assembled because Python's `zlib` cannot emit the optional fields, and they are the
    whole point of this seed: `inflateGetHeader`'s `extra_max`/`name_max`/`comm_max` clamps
    are what `fuzz_inflate`'s `HeaderProbe` exists to police, and they are unreachable
    without a member that actually carries the fields.
    """
    extra_payload = b"AB" + bytes([4, 0]) + b"seed"
    name = b"seed-with-fields.txt\x00"
    comment = b"a gzip member carrying every optional field\x00"
    # FEXTRA | FNAME | FCOMMENT | FHCRC
    flags = 0x04 | 0x08 | 0x10 | 0x02
    header = bytes([0x1F, 0x8B, 0x08, flags]) + struct.pack("<I", 0) + bytes([0x00, 0x03])
    header += struct.pack("<H", len(extra_payload)) + extra_payload + name + comment
    header += struct.pack("<H", zlib.crc32(header) & 0xFFFF)
    return header + raw_stream(data) + struct.pack("<II", zlib.crc32(data), len(data) & 0xFFFF_FFFF)


def zlib_stream_with_dictionary(data: bytes, dictionary: bytes) -> bytes:
    """A zlib stream compressed against a preset dictionary, so FDICT is set."""
    engine = zlib.compressobj(6, zlib.DEFLATED, 15, 8, 0, dictionary)
    return engine.compress(data) + engine.flush()


# ---------------------------------------------------------------------------
#  Layout checking
# ---------------------------------------------------------------------------

FUZZ_DIR = Path(__file__).resolve().parents[1]
TARGET_DIR = FUZZ_DIR / "fuzz_targets"
SEED_DIR = Path(__file__).resolve().parent


def source_fields(target: str, record: str) -> list[str]:
    """The field names of `record`, in declaration order, read out of the target's source."""
    source = (TARGET_DIR / f"{target}.rs").read_text(encoding="utf-8")
    pattern = re.compile(
        r"struct\s+" + re.escape(record) + r"(?:<[^>]*>)?\s*\{(?P<body>.*?)\n\}", re.S
    )
    found = pattern.search(source)
    if found is None:
        raise SystemExit(f"error: struct {record} not found in {target}.rs")
    fields = []
    for line in found.group("body").splitlines():
        stripped = line.strip()
        if not stripped or stripped.startswith(("//", "#[", "///")):
            continue
        name = stripped.split(":", 1)[0].strip()
        if name.isidentifier():
            fields.append(name)
    return fields


def check_layout(target: str, record: str, expected: list[str]) -> None:
    """Requires the encoder's field list to match the record's, or exits explaining."""
    actual = source_fields(target, record)
    if actual == expected:
        return
    raise SystemExit(
        f"error: {target}.rs's `{record}` no longer has the field order this script encodes.\n"
        f"       source:  {actual}\n"
        f"       encoder: {expected}\n"
        f"       Field order IS the wire format for an `arbitrary`-decoded record, so every\n"
        f"       seed for this target is now meaningless. Update the encoder below to match,\n"
        f"       regenerate, and re-run the CI seed gate to confirm the new seeds still reach\n"
        f"       their productive `reached=` value."
    )


# ---------------------------------------------------------------------------
#  Per-target encoders
# ---------------------------------------------------------------------------

INFLATE_FIELDS = [
    "window_bits",
    "reset_window_bits",
    "out_len",
    "step",
    "alloc_limit",
    "flush",
    "prime_bits",
    "prime_value",
    "ops",
    "header_caps",
    "dictionary",
    "payload",
]

#: `fuzz_inflate`'s `ops` bits, mirrored from its `mod ops`.
INFLATE_OPS = {
    "COPY": 1 << 0,
    "GET_HEADER": 1 << 1,
    "GET_DICTIONARY": 1 << 2,
    "SET_DICTIONARY": 1 << 3,
    "SYNC": 1 << 4,
    "PRIME": 1 << 5,
    "FLAGS": 1 << 6,
    "RESET": 1 << 7,
}


def inflate_seed(
    *,
    window_bits: int,
    payload: bytes,
    out_len: int = 512,
    step: int = 0,
    flush: int = 0,
    ops: int = 0,
    header_caps: tuple[int, int, int] = (0, 0, 0),
    dictionary: bytes = b"",
    reset_window_bits: int = 0,
) -> bytes:
    """One `InflateInput`.

    `window_bits` and `reset_window_bits` are the RAW selector bytes, not `windowBits`
    values: the target maps them through `window_bits_for`, where `0` means 47 (zlib-or-gzip
    auto-detect, the only request that permits header collection), `19` means 15 (zlib) and
    `27` means -15 (raw). `alloc_limit` is pinned to the selector `1` in every seed, because
    `alloc_limit_for` imposes a ceiling exactly when the selector is a multiple of eight and
    a ceiling that tight refuses `inflateInit2_` outright -- which is how an all-zero file
    manages to reach nothing at all.
    """
    return (
        u8(window_bits)
        + u8(reset_window_bits)
        + u16(out_len)
        + u16(step)
        + u16(1)
        + u8(flush)
        + i8(0)
        + i32(0)
        + u16(ops)
        + array_u8(bytes(header_caps), 3)
        + vec_u8(dictionary)
        + vec_u8_take_rest(payload)
    )


BACK_FIELDS = [
    "window_bits",
    "chunk",
    "staged",
    "failure",
    "starve_input",
    "tracked_allocator",
    "reuse_state",
    "payload",
]


def back_seed(
    *,
    window_bits: int,
    payload: bytes,
    chunk: int = 0,
    staged: int = 0,
    failure: int = 0,
    starve_input: bool = False,
    tracked_allocator: bool = True,
    reuse_state: bool = False,
) -> bytes:
    """One `BackInput`.

    `payload` is verbatim: it is the final field and a `&[u8]`, so `arbitrary_take_rest` is
    `take_rest` and no continuation bytes are involved. `failure` is the `FailureMode`
    variant index -- 0 is `Never`, which is what a productive seed wants.
    """
    return (
        u8(window_bits)
        + u8(chunk)
        + u8(staged)
        + enum_variant(failure, 3)
        + boolean(starve_input)
        + boolean(tracked_allocator)
        + boolean(reuse_state)
        + slice_take_rest(payload)
    )


DEFLATE_CONFIG_FIELDS = [
    "mode",
    "level",
    "method",
    "window_bits",
    "mem_level",
    "strategy",
    "extreme",
]

DEFLATE_FIELDS = [
    "config",
    "ops",
    "chunk",
    "out_buf",
    "alloc_limit",
    "flush_schedule",
    "params_level",
    "params_strategy",
    "prime_bits",
    "prime_value",
    "tune",
    "header_flags",
    "header_time",
    "header_extra",
    "header_name",
    "header_comment",
    "dictionary",
    "payload",
]

#: The `window_bits` SELECTOR values that `legal_window_bits` maps onto each container.
#: `spread % 3` picks the family and `spread % 8` or `spread % 7` the exponent, so these are
#: solutions rather than the values themselves: 15 -> 8 + 15 % 8 = 15 (zlib), 13 -> -(9 + 13 % 7)
#: = -15 (raw), 20 -> 25 + 20 % 7 = 31 (gzip).
DEFLATE_WINDOW_SELECTOR = {"zlib": 15, "raw": 13, "gzip": 20}


def deflate_seed(
    *,
    container: str,
    payload: bytes,
    level: int = 6,
    strategy: int = 0,
    mem_level_selector: int = 7,
    ops: int = 0,
    chunk: int = 1023,
    out_buf: int = 4095,
    dictionary: bytes = b"",
    header_name: bytes = b"",
    header_comment: bytes = b"",
) -> bytes:
    """One `FuzzInput`.

    `mode` is pinned to `1` so `RawConfig::kind` answers `DrawKind::Legal`: the boundary and
    unrestricted draws exist to be REFUSED, and a seed corpus made of refusals is the defect
    these files fix. `out_buf` avoids multiples of eight, which `out_len` forces to a
    single-byte output window, and `alloc_limit` is even so `alloc_limit()` answers "no
    ceiling".

    `chunk` defaults high, and that was MEASURED rather than chosen. `chunk_len()` is
    `1 + chunk % 4096`, so `chunk = 0` feeds ONE BYTE PER CALL; against `MAX_ITERATIONS = 256`
    that cannot finish a payload of more than a few hundred bytes, and the 400-byte RLE seed
    came back `reached=not_finished` with two bytes produced. 1023 offers the whole payload at
    once, which is what a baseline seed should do; the seed that wants chunking asks for it
    explicitly.
    """
    config = (
        u8(1)
        + i8(level)
        + i8(0)
        + i8(DEFLATE_WINDOW_SELECTOR[container])
        + i8(mem_level_selector)
        + i8(strategy)
        + array_i32([], 5)
    )
    return (
        config
        + u16(ops)
        + u16(chunk)
        + u16(out_buf)
        + u32(0)
        + array_u8(bytes(8), 8)
        + i8(level)
        + i8(strategy)
        + i8(0)
        + i32(0)
        + array_i16([], 4)
        + u8(0)
        + u32(0)
        + array_u8(b"", 16)
        + array_u8(header_name, 16)
        + array_u8(header_comment, 16)
        + vec_u8(dictionary)
        + vec_u8_take_rest(payload)
    )


GZ_FIELDS = [
    "write_shape",
    "read_shape",
    "canonical_pick",
    "write_seed",
    "read_seed",
    "seed_lengths",
    "buffer_size",
    "level",
    "strategy",
    "write_chunk",
    "read_chunk",
    "gets_len",
    "item_size",
    "seek_target",
    "seek_delta",
    "write_skip",
    "flags",
    "ops",
    "payload",
]

#: `MAX_MODE_SEED` in `fuzz_gz_roundtrip.rs`.
GZ_MODE_SEED = 8


def gz_seed(
    *,
    payload: bytes,
    write_shape: int,
    read_shape: int,
    canonical_pick: int = 0,
    level: int = 6,
    strategy: int = 0,
    write_chunk: int = 0,
    read_chunk: int = 0,
    buffer_size: int = 8192,
    ops: bytes = b"",
    flags: int = 0,
) -> bytes:
    """One `GzRoundTrip`."""
    return (
        u8(write_shape)
        + u8(read_shape)
        + u8(canonical_pick)
        + array_u8(bytes(GZ_MODE_SEED), GZ_MODE_SEED)
        + array_u8(bytes(GZ_MODE_SEED), GZ_MODE_SEED)
        + u8(0)
        + u16(buffer_size)
        + i8(level)
        + u8(strategy)
        + u16(write_chunk)
        + u16(read_chunk)
        + u8(0)
        + u8(1)
        + u16(0)
        + u16(0)
        + u8(0)
        + u32(flags)
        + vec_u8(ops)
        + vec_u8_take_rest(payload)
    )


CHECKSUM_FIELDS = [
    "chunk",
    "split",
    "adler_seed",
    "crc_seed",
    "synthetic_crc1",
    "synthetic_crc2",
    "negative_magnitude",
    "check_tables",
    "probe_resumption",
    "payload",
]


def checksum_seed(
    *,
    payload: bytes,
    chunk: int = 7,
    split: int = 3,
    adler_seed: int = 1,
    crc_seed: int = 0,
    check_tables: bool = True,
    probe_resumption: bool = True,
) -> bytes:
    """One `ChecksumInput`. `payload` is verbatim -- final field, `&[u8]`, `take_rest`."""
    return (
        u8(chunk)
        + u16(split)
        + u32(adler_seed)
        + u32(crc_seed)
        + u32(0xDEAD_BEEF)
        + u32(0x1234_5678)
        + u16(64)
        + boolean(check_tables)
        + boolean(probe_resumption)
        + slice_take_rest(payload)
    )


# ---------------------------------------------------------------------------
#  The corpus
# ---------------------------------------------------------------------------


@dataclass(frozen=True)
class Seed:
    """One committed seed: where it goes, what is in it, and what it is for."""

    target: str
    name: str
    body: bytes
    purpose: str


def corpus() -> list[Seed]:
    """Every committed seed, after checking that each record's field order still holds."""
    check_layout("fuzz_inflate", "InflateInput", INFLATE_FIELDS)
    check_layout("fuzz_inflate_back", "BackInput", BACK_FIELDS)
    check_layout("fuzz_deflate", "RawConfig", DEFLATE_CONFIG_FIELDS)
    check_layout("fuzz_deflate", "FuzzInput", DEFLATE_FIELDS)
    check_layout("fuzz_gz_roundtrip", "GzRoundTrip", GZ_FIELDS)
    check_layout("fuzz_checksum", "ChecksumInput", CHECKSUM_FIELDS)

    seeds: list[Seed] = []

    # --- fuzz_inflate -----------------------------------------------------
    seeds += [
        Seed(
            "fuzz_inflate",
            "zlib_autodetect.bin",
            inflate_seed(window_bits=0, payload=zlib_stream(HELLO)),
            "a complete RFC 1950 stream under the auto-detect request",
        ),
        Seed(
            "fuzz_inflate",
            "raw_deflate.bin",
            inflate_seed(window_bits=27, payload=raw_stream(PROSE)),
            "a bare RFC 1951 stream at windowBits -15, the no-wrapper path",
        ),
        Seed(
            "fuzz_inflate",
            "gzip_autodetect.bin",
            inflate_seed(window_bits=0, payload=gzip_stream(PROSE)),
            "an RFC 1952 member reached through auto-detect",
        ),
        Seed(
            "fuzz_inflate",
            "gzip_header_fields.bin",
            inflate_seed(
                window_bits=0,
                payload=gzip_stream_with_fields(PROSE),
                ops=INFLATE_OPS["GET_HEADER"],
                header_caps=(48, 48, 48),
            ),
            "a member carrying FEXTRA/FNAME/FCOMMENT/FHCRC with inflateGetHeader armed, so "
            "the extra_max/name_max/comm_max clamps are exercised",
        ),
        Seed(
            "fuzz_inflate",
            "zlib_chunked.bin",
            inflate_seed(window_bits=19, payload=zlib_stream(PROSE), step=3, out_len=17),
            "the same stream fed three bytes at a time into a 17-byte output window, so the "
            "resumption paths run instead of one single-shot call",
        ),
        Seed(
            "fuzz_inflate",
            "zlib_preset_dictionary.bin",
            inflate_seed(
                window_bits=19,
                payload=zlib_stream_with_dictionary(HELLO, DICTIONARY),
                dictionary=DICTIONARY,
                ops=INFLATE_OPS["SET_DICTIONARY"] | INFLATE_OPS["GET_DICTIONARY"],
            ),
            "an FDICT stream with the matching dictionary, so Z_NEED_DICT and "
            "inflateSetDictionary are both reached",
        ),
    ]

    # --- fuzz_inflate_back ------------------------------------------------
    # `window_bits_for` in that target maps the selector onto 8..=15; the payload must be a
    # bare DEFLATE stream, because inflateBack takes no wrapper at all.
    seeds += [
        Seed(
            "fuzz_inflate_back",
            "raw_single_chunk.bin",
            back_seed(window_bits=15, payload=raw_stream(PROSE)),
            "a complete raw stream handed over in one chunk",
        ),
        Seed(
            "fuzz_inflate_back",
            "raw_chunked.bin",
            back_seed(window_bits=15, payload=raw_stream(PROSE), chunk=5, staged=3),
            "the same stream delivered five bytes per in() call, so the callback loop runs "
            "many times",
        ),
        Seed(
            "fuzz_inflate_back",
            "raw_stored_block.bin",
            back_seed(window_bits=15, payload=raw_stream(bytes(600), level=0)),
            "a stored-block stream, which is the copy path rather than the Huffman one",
        ),
        Seed(
            "fuzz_inflate_back",
            "raw_reused_state.bin",
            back_seed(window_bits=15, payload=raw_stream(HELLO), reuse_state=True),
            "one stream decoded twice through the same state, which is the reset path",
        ),
    ]

    # --- fuzz_deflate -----------------------------------------------------
    seeds += [
        Seed(
            "fuzz_deflate",
            "zlib_level6.bin",
            deflate_seed(container="zlib", payload=PROSE),
            "an ordinary level-6 zlib compression that runs to Z_STREAM_END",
        ),
        Seed(
            "fuzz_deflate",
            "raw_level9.bin",
            deflate_seed(container="raw", payload=PROSE, level=9),
            "level 9 with no wrapper, the deepest chain search",
        ),
        Seed(
            "fuzz_deflate",
            "gzip_level1_header.bin",
            deflate_seed(
                container="gzip",
                payload=PROSE,
                level=1,
                ops=(1 << 5) | (1 << 3),
                header_name=b"seed.txt",
                header_comment=b"a gzip header",
            ),
            "level 1 into a gzip container with deflateSetHeader armed on both sessions",
        ),
        Seed(
            "fuzz_deflate",
            "zlib_dictionary.bin",
            deflate_seed(
                container="zlib",
                payload=HELLO,
                dictionary=DICTIONARY,
                ops=(1 << 4) | (1 << 2) | (1 << 15),
            ),
            "a preset dictionary set on both sessions and read back, mirroring "
            "test/example.c's dictionary test",
        ),
        Seed(
            "fuzz_deflate",
            "zlib_chunked_tiny_output.bin",
            deflate_seed(container="zlib", payload=PROSE, chunk=7, out_buf=8),
            "seven bytes in per call and a one-byte output window, which drains the pending "
            "buffer through flush_pending one byte at a time",
        ),
        Seed(
            "fuzz_deflate",
            "zlib_rle_strategy.bin",
            deflate_seed(container="zlib", payload=bytes([0x41]) * 400, strategy=3),
            "Z_RLE over a highly repetitive payload, the strategy the default draws reach "
            "least often",
        ),
    ]

    # --- fuzz_gz_roundtrip ------------------------------------------------
    # `write_shape`/`read_shape` are selectors into that target's mode grammar; the
    # `canonical_pick` fallback is what guarantees a usable pair, so the seeds vary the
    # payload and the chunking rather than gambling on shape bytes.
    seeds += [
        Seed(
            "fuzz_gz_roundtrip",
            "write_then_read.bin",
            gz_seed(payload=PROSE, write_shape=0, read_shape=0),
            "the canonical write-then-read round trip",
        ),
        Seed(
            "fuzz_gz_roundtrip",
            "write_then_read_chunked.bin",
            gz_seed(payload=PROSE, write_shape=0, read_shape=0, write_chunk=9, read_chunk=5),
            "the same round trip in nine-byte writes and five-byte reads",
        ),
        Seed(
            "fuzz_gz_roundtrip",
            "empty_member.bin",
            gz_seed(payload=b"", write_shape=0, read_shape=0),
            "an empty member, which is a legal gzip file and the shortest one",
        ),
        Seed(
            "fuzz_gz_roundtrip",
            "level0_stored.bin",
            gz_seed(payload=PROSE, write_shape=0, read_shape=0, level=0),
            "level 0, so the member holds stored blocks",
        ),
    ]

    # --- fuzz_checksum ----------------------------------------------------
    seeds += [
        Seed(
            "fuzz_checksum",
            "prose.bin",
            checksum_seed(payload=PROSE),
            "a payload past the 32-byte bytewise prefix, so the chunked, resumed and "
            "single-shot answers must all agree",
        ),
        Seed(
            "fuzz_checksum",
            "long_run.bin",
            checksum_seed(payload=bytes(range(256)) + bytes([0xFF]) * 200, chunk=31, split=97),
            "512 bytes with an awkward chunk and split, which is where a wrong NMAX "
            "wrap-around would show",
        ),
        Seed(
            "fuzz_checksum",
            "empty.bin",
            checksum_seed(payload=b""),
            "the empty slice, whose Adler-32 and CRC-32 are defined and easy to get wrong",
        ),
    ]

    return seeds


def main() -> int:
    """Write or verify every seed."""
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument(
        "--check",
        action="store_true",
        help="verify the committed seeds against this script and write nothing",
    )
    arguments = parser.parse_args()

    seeds = corpus()
    expected: dict[Path, bytes] = {
        SEED_DIR / seed.target / seed.name: seed.body for seed in seeds
    }

    if arguments.check:
        problems: list[str] = []
        for path, body in sorted(expected.items()):
            relative = path.relative_to(SEED_DIR.parent)
            if not path.is_file():
                problems.append(f"{relative} is missing")
            elif path.read_bytes() != body:
                problems.append(f"{relative} differs from what this script generates")
        for path in sorted(SEED_DIR.rglob("*.bin")):
            if path not in expected:
                problems.append(
                    f"{path.relative_to(SEED_DIR.parent)} is not accounted for by this script"
                )
        if problems:
            print("error: the committed seed corpus does not match generate.py:")
            for problem in problems:
                print(f"  - {problem}")
            print(
                "       Regenerate with `python3 fuzz/seeds/generate.py` and commit the\n"
                "       result. If a record's fields moved, the encoder above has to move\n"
                "       with them -- an `arbitrary` record's field order IS its wire format."
            )
            return 1
        print(f"PASS: {len(expected)} committed seed(s) match fuzz/seeds/generate.py")
        return 0

    for path, body in sorted(expected.items()):
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(body)
    by_target: dict[str, int] = {}
    for seed in seeds:
        by_target[seed.target] = by_target.get(seed.target, 0) + 1
    for target in sorted(by_target):
        print(f"{target}: {by_target[target]} seed(s)")
    print(f"wrote {len(expected)} seed(s) under {SEED_DIR.relative_to(SEED_DIR.parent.parent)}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
