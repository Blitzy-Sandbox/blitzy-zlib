# `fuzz/seeds` — one starting corpus per target, and proof that each one works

Twenty-three committed files, grouped by the target that consumes them. They exist because
of a specific defect: all six CI fuzzing rows used to seed from the same directory,
`crates/zlib-rs-differential/corpus/minimal/`, which holds **uncompressed** fixtures.

Those fixtures are excellent DEFLATE inputs. As *typed records* they are close to
worthless. Each target takes an `arbitrary`-decoded struct — `InflateInput`, `BackInput`,
`FuzzInput`, `GzRoundTrip`, `ChecksumInput` — and `arbitrary` never fails: a file that is
not a sensible encoding of the record decodes to a record whose `windowBits` is outside the
legal space, or whose payload is empty, and the execution stops in the first refusal. So
every row started from a corpus of immediate error paths, and nothing in CI could tell that
apart from a row decoding real streams.

## What is here

| Target | Seeds | What they carry |
|---|---|---|
| `fuzz_inflate` | 6 | complete zlib, gzip and raw streams; a member with every optional gzip field and `inflateGetHeader` armed; a chunked stream into a 17-byte window; an `FDICT` stream with its dictionary |
| `fuzz_inflate_back` | 4 | raw streams in one chunk and in five-byte chunks; a stored-block stream; one decoded twice through a reset state |
| `fuzz_deflate` | 6 | zlib/raw/gzip at levels 1, 6 and 9; a preset dictionary; a one-byte output window; `Z_RLE` over repetitive input |
| `fuzz_gz_roundtrip` | 4 | write-then-read, chunked, empty member, level 0 |
| `fuzz_checksum` | 3 | prose, 456 bytes with an awkward chunk and split, and the empty slice |

## They are generated, and verified from the other side

`generate.py` writes every one of them and encodes the records to `arbitrary` 1.4.2's own
rules — which are not obvious; the header of that file sets them out, including the one that
matters most: a `Vec<u8>` costs **two bytes per element** even in the final field, because
`ArbitraryTakeRestIter` reads a continuation `bool` before each one. That single rule is why
a compressed stream appended to a plausible header decodes to about half of itself, and why
these files had to be encoded rather than assembled by hand.

Being generated is not on its own evidence that they *work*. So the claim is checked from the
opposite direction as well. Every target prints one line per execution when
`ZLIB_RS_FUZZ_REPORT` is set:

```text
FUZZ-REACHED target=fuzz_inflate reached=stream_end windowBits=47 payload=19 consumed=19 produced=14
```

`.github/workflows/rust.yml`'s `fuzz` job runs each committed seed through its target once
with that variable set and requires the target's productive `reached=` value to appear at
least once. Measured on the committed corpus: every `fuzz_inflate`, `fuzz_inflate_back` and
`fuzz_deflate` seed reaches `stream_end`, every `fuzz_gz_roundtrip` seed reaches
`round_trip`, and two of the three `fuzz_checksum` seeds reach `compared` — the third is the
empty slice, deliberately.

The two halves catch different things. `--check` catches a seed edited or removed without
regenerating. The `reached=` gate catches the failure that matters more and that no encoder
can detect: a record whose fields were reordered, which silently turns every seed for that
target back into noise.

## Maintenance

```bash
python3 fuzz/seeds/generate.py            # rewrite the seeds
python3 fuzz/seeds/generate.py --check    # verify them; what CI runs
```

`generate.py` reads each record's field list out of the target's source and refuses to run if
it no longer matches what it encodes, so a reordered record fails here with both lists
printed rather than producing seeds that decode to nothing.

CI copies these into `fuzz/corpus/<target>/` without overwriting anything the corpus cache
restored, so an evolved corpus is never thrown away — the seeds are a floor, not a
replacement.
