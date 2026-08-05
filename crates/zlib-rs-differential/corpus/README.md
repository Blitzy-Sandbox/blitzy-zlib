# Differential test corpus

This folder holds the input data for every correctness gate in
`crates/zlib-rs-differential/tests/`, plus the pointer to the optional large-input tier
that the repository-root benchmark suites measure against. It is pure data, one shell
script and this README — there is no Rust here, nothing in this folder is compiled, and
nothing in it participates in a build. The differential harness that consumes it lives one
directory up ([`../Cargo.toml`](../Cargo.toml)); this folder only supplies the bytes that
harness feeds to the Rust port and to the in-tree C oracle so their outputs can be compared.

Because the port's governing acceptance criterion is that compressed output be
*byte-identical* to the C reference, this corpus is not incidental test data. **It is the
definition of the sample over which that claim is measured.** Everything below —
the filenames, the sizes, the byte-content requirements, the determinism rule — is a
contract, and the test and benchmark files that read this folder are written to conform to
it.

## Contents

```
corpus/
├── README.md            this file — the published contract
├── fetch_silesia.sh     opt-in, manual, never run by cargo test or CI
└── minimal/             the ten committed fixtures listed below
```

## The two tiers, and why there are two

The corpus is deliberately split into two tiers with completely different rules. This is
the design decision recorded in AAP §0.6.4.4, and its resolution of AAP §0.8.2
ambiguity #5 (Silesia acquisition was left unspecified by the requirements).

| | Tier 1 — `minimal/` | Tier 2 — Silesia |
| --- | --- | --- |
| **Committed to git?** | Yes, every byte | No, never |
| **How obtained** | Already here | `fetch_silesia.sh`, run by hand |
| **Used by** | All correctness gates | Benchmarks only |
| **Network needed** | Never | Yes, at fetch time only |
| **Required for `cargo test`** | Yes | No — absence is not a failure |

**Tier 1 is the sole input to every correctness gate.** The byte-identity matrix, the
round-trip interoperability tests and the table-equality checks all read `minimal/` and
nothing else. That is a hard requirement rather than a convenience: `cargo test` and CI must
never touch the network, so no correctness gate may depend on data that has to be
downloaded. A gate that reached for Silesia would be a gate that silently passes on a
machine with no network — or worse, one whose result depends on what a remote server served
that day.

**Tier 2 exists only to answer "is it fast enough?".** Throughput is measured on realistic,
large, heterogeneous input, and Silesia is the standard corpus for that. It is fetched by
explicit human action and it is never redistributed here — see
[Licensing](#licensing) and [The Silesia tier](#the-silesia-tier-path-contract).

This corpus adds no tooling and no crate to the workspace. `fetch_silesia.sh` relies only
on ubiquitous shell utilities, and the fixtures are read with ordinary file I/O — no
dependency exists, or may be added, to serve this folder.

## Fixture inventory — the contract

The tests load these files **by exact filename**. There is no globbing, no directory scan
and no fallback: a name that does not match is a missing file, not a skipped case.
**Renaming, moving or removing a fixture is therefore a breaking change and must be made in
the same commit as the change to every consumer that reads it.**

These ten files are the whole of tier 1. There are no others.

| Fixture | Size | What it exercises | Requiring reference |
| --- | --- | --- | --- |
| `minimal/empty.bin` | 0 bytes | Zero-length deflate and inflate; `Z_BUF_ERROR` boundaries; the empty stored block; the empty-input classification boundary | [`doc/txtvsbin.txt`](../../../doc/txtvsbin.txt) L48-51; [`trees.c`](../../../trees.c) L987-990 |
| `minimal/single_byte.bin` | 1 byte | The degenerate Huffman case — one distinct symbol forces the two-code fixup | [`trees.c`](../../../trees.c) L650-661 |
| `minimal/repetitive.bin` | small | Long matches, `nice_length` / `max_chain` saturation, and the `Z_RLE` strategy | [`doc/algorithm.txt`](../../../doc/algorithm.txt) L7-8, L21-22, L27-28, L32-34 |
| `minimal/random.bin` | small | Incompressible input; forces stored-block selection | [`trees.c`](../../../trees.c) L1047-1048 |
| `minimal/text.txt` | small | Drives the data-type heuristic to `Z_TEXT` | [`doc/txtvsbin.txt`](../../../doc/txtvsbin.txt) L41-51; [`trees.c`](../../../trees.c) L980-985 |
| `minimal/binary.bin` | small | Drives the data-type heuristic to `Z_BINARY` through the block-list loop | [`doc/txtvsbin.txt`](../../../doc/txtvsbin.txt) L45-46; [`trees.c`](../../../trees.c) L971-977 |
| `minimal/gray_list.bin` | small | Drives the data-type heuristic to `Z_BINARY` through the *final fall-through* — a distinct path | [`doc/txtvsbin.txt`](../../../doc/txtvsbin.txt) L43-44; [`trees.c`](../../../trees.c) L987-990 |
| `minimal/window_boundary.bin` | > 32768 bytes | Window refill and hash sliding; the maximum-distance cutoff | [`doc/algorithm.txt`](../../../doc/algorithm.txt) L7-9; `deflate.c` `fill_window` / `slide_hash` |
| `minimal/hello.bin` | exactly 14 bytes | The payload the existing C suite compresses, including its dictionary case | [`test/example.c`](../../../test/example.c) L35 |
| `minimal/dictionary.bin` | exactly 6 bytes | The preset dictionary the existing C suite sets, and the `dictId` check | [`test/example.c`](../../../test/example.c) L40 |

"small" means the fixture's exact length is not pinned by this contract — only its
*content class* is. The four pinned sizes (0, 1, 14 and 6 bytes) and the one pinned floor
(> 32768 bytes) are load-bearing and must not be changed.

> **On the line references used throughout this document.** Every `Lnnn` citation below
> points into the C sources as they stand at this tree's current revision, and each is there
> so a reader can confirm a claim rather than take it on trust. The referenced files are
> read-only for this port, so the numbers are stable; if one ever drifts, the source is
> right and this document is wrong. Correct the document.

## Rationale, fixture by fixture

Each fixture exists because it reaches code that no other fixture reaches. None is
decorative, and none is a near-duplicate of another.

### `empty.bin` — 0 bytes

Zero-length input is the boundary every streaming codec gets wrong first. It exercises a
`deflate` call with `avail_in == 0`, the emission of a well-formed stream containing no
literal data, the corresponding `inflate` returning `Z_STREAM_END` without producing
output, and the `Z_BUF_ERROR` conditions that distinguish "no progress possible" from "no
input supplied". It is also the classification boundary case: an empty input carries no
allow-listed byte, so the heuristic reports binary rather than text
([`doc/txtvsbin.txt`](../../../doc/txtvsbin.txt) L48-51, and the fall-through comment at
[`trees.c`](../../../trees.c) L987-989 which names the empty stream explicitly).

### `single_byte.bin` — 1 byte

A single distinct symbol produces a Huffman tree with fewer than two leaves, which the
format does not permit. `build_tree` ([`trees.c`](../../../trees.c) L627) therefore
force-creates codes until at least two exist, in the loop at L655-661. The comment above that
loop, at L650-654, gives the reason: the pkzip format requires that "at least one distance
code exists", and that at least one bit be sent even when only one code is possible. This is
a special case a clean-room implementation would plausibly handle differently, and a one-byte
input is the shortest way to reach it.

### `repetitive.bin` — highly repetitive

Long runs drive the match finder into its saturation behaviour: matches reach the
258-byte ceiling, `nice_length` short-circuits the chain walk, and `max_chain` truncation
becomes observable. It is also the natural input for the `Z_RLE` strategy. Three facts from
[`doc/algorithm.txt`](../../../doc/algorithm.txt) set the shape of what this fixture must
provoke: match distances are capped at 32K and match lengths at 258 bytes (L7-8), every
input string of length 3 is entered into the hash table (L21-22), and very long hash chains
are truncated at a length determined by the level passed to `deflateInit` (L32-34). Because
chains are walked most-recent-first to favour short distances (L27-28), repeated content is
also what makes the traversal order observable at all. To reach saturation rather than
merely brush it, the fixture needs to be at least a few kibibytes of repeating content, not
a handful of bytes.

### `random.bin` — incompressible

When the entropy-coded encoding of a block would be no smaller than the raw bytes, the
encoder must emit a *stored* block instead. The decision is the integer comparison at
[`trees.c`](../../../trees.c) L1047 — `stored_len + 4 <= opt_lenb && buf != (char*)0`, whose
trailing comment at L1048 explains the constant as "4: two words for the lengths" — sitting
below the two byte-length computations at L1027-1028, `opt_lenb = (s->opt_len + 3 + 7) >> 3`
and `static_lenb = (s->static_len + 3 + 7) >> 3`. Incompressible input is the only way to
make that branch taken rather than merely evaluated. Note that these are truncating integer
comparisons, so the fixture also guards against any reimplementation that reorders the
arithmetic or reaches for floating point.

**This fixture is committed random bytes, not bytes generated at test time.** See
[Determinism](#determinism-is-a-requirement-not-a-preference).

### `text.txt` — natural-language text

Drives the data-type heuristic to `Z_TEXT`. For that to happen the fixture must contain at
least one allow-listed byte *and* zero block-listed bytes. Practically: ordinary printable
text, and among the control bytes only TAB, LF and CR. **No NUL, and no other control
byte.** A single stray control byte silently converts this fixture into a second binary
fixture, and the `Z_TEXT` path would then never be measured by anything.

### `binary.bin` — binary data

Drives the data-type heuristic to `Z_BINARY` via the block-list loop at
[`trees.c`](../../../trees.c) L971-977. It must contain at least one byte drawn from
`0x00-0x06`, `0x0E-0x19` or `0x1C-0x1F`; without one, the loop finds nothing, the fixture
falls through to the allow-list check and is classified as text. Including a NUL is the
simplest way to satisfy this and matches the observation that binary files tend to contain
NUL.

### `gray_list.bin` — tolerated control bytes only

Contains **only** the six gray-listed bytes: `0x07` (BEL), `0x08` (BS), `0x0B` (VT),
`0x0C` (FF), `0x1A` (SUB), `0x1B` (ESC). Such input has no block-listed byte, so the loop at
[`trees.c`](../../../trees.c) L971-977 does not fire; and it has no allow-listed byte, so
neither of the checks at L980-985 fires either. It reaches the final `return Z_BINARY` at
L990 — the third exit from the function, guarded by the comment at L987-989 about a stream
that "either is empty or has tolerated (\"gray-listed\") bytes only".

**This is not a duplicate of `binary.bin`.** Both fixtures end at `Z_BINARY`, but by
different exits: `binary.bin` returns early from the block-list loop, `gray_list.bin` runs
both loops to completion and falls off the end. A port that dropped the fall-through and
returned `Z_TEXT` by default would still pass on `binary.bin` and fail only here.

### `window_boundary.bin` — larger than the window

Must exceed 32768 bytes, so that compression cannot complete within a single window's worth
of history. That forces the window-refill and hash-sliding paths (`fill_window` and
`slide_hash` in `deflate.c`) to run at least once, and it makes the maximum-distance cutoff
observable — `MAX_DIST` is defined in `deflate.h` as the window size less `MIN_LOOKAHEAD`,
so with a 32 KiB window the furthest usable distance is 32506 rather than 32768. Matches
that would reach further back must be rejected, and that rejection is only reachable once
the input outgrows the window. The 32K figure is the distance limit stated in
[`doc/algorithm.txt`](../../../doc/algorithm.txt) L7-9.

### `hello.bin` and `dictionary.bin`

These two are not synthetic. They are the exact literals the existing C test suite uses, and
they get their own section below because their lengths are easy to get wrong.

## The byte-classification categories

The heuristic that assigns `Z_TEXT` or `Z_BINARY` partitions all 256 byte values into three
categories. `text.txt`, `binary.bin` and `gray_list.bin` exist to cover one category each.

| Category | Bytes (decimal) | Bytes (hex) | Effect on classification |
| --- | --- | --- | --- |
| Allow list | 9, 10, 13, 32-255 | `0x09`, `0x0A`, `0x0D`, `0x20-0xFF` | One or more, with no block-listed byte, yields `Z_TEXT` |
| Gray list | 7, 8, 11, 12, 26, 27 | `0x07`, `0x08`, `0x0B`, `0x0C`, `0x1A`, `0x1B` | Ignored — neither forces nor prevents either verdict |
| Block list | 0-6, 14-25, 28-31 | `0x00-0x06`, `0x0E-0x19`, `0x1C-0x1F` | Any one yields `Z_BINARY` immediately |

The categories are described in [`doc/txtvsbin.txt`](../../../doc/txtvsbin.txt) L41-46 and
implemented in `detect_data_type` at [`trees.c`](../../../trees.c) L966-991, where the block
list is encoded as the bit mask `0xf3ffc07fUL` (L971).

### A deliberate divergence — do not "fix" the mask

The block-list row above follows the **implementation**, and the implementation differs from
the prose document by exactly two bytes.

- [`doc/txtvsbin.txt`](../../../doc/txtvsbin.txt) L45-46 states the block list as 0 to 6
  and 14 to 31 — which would include 26 (SUB) and 27 (ESC).
- The mask at [`trees.c`](../../../trees.c) L971 sets bits 0-6, 14-25 and 28-31 only. Bits
  26 and 27 are clear, so SUB and ESC are **not** block-listed. The comment at L967-970
  states the intent and writes the mask out in binary as
  `11110011111111111100000001111111`, and the function's own doc comment at L955-963 states
  the same narrower block list. Expanding `0xf3ffc07f` bit by bit confirms it.

Treating SUB and ESC as tolerated rather than forbidden is consistent with
`doc/txtvsbin.txt`'s *own* gray list at L43-44, which names both. The two documents disagree
about which of its lists those bytes belong to; the implementation follows the gray list.

**This divergence is intentional and is load-bearing for this corpus.** It is precisely what
makes `gray_list.bin` reach the fall-through: were bits 26 and 27 set, a fixture containing
SUB or ESC would return early from the block-list loop and the third exit would become
unreachable. A future contributor who "corrects" the mask to match the prose will change
emitted `data_type` values and turn one of these fixtures into dead weight.

## Provenance of the two `test/example.c` literals

`hello.bin` and `dictionary.bin` are transcriptions of literals in
[`test/example.c`](../../../test/example.c). They exist so the Rust differential harness
exercises the *same* bytes the existing C suite has always exercised, which makes a
disagreement between the two attributable to the implementation rather than to the input.

This is the most precise section in the document, because both lengths are off-by-one traps.

### `minimal/hello.bin` — exactly 14 bytes

Declared at [`test/example.c`](../../../test/example.c) L35:

```c
static z_const char hello[] = "hello, hello!";
```

- **Content:** the 13 characters `hello, hello!` followed by one NUL terminator.
- **Length: 14 bytes, not 13.** Every site in the C suite that compresses this payload
  passes `strlen(hello)+1` as the length — L69, L95, L175, L341 and L434 — so the trailing
  NUL is part of the data being compressed, not merely part of the C string. `hello.bin`
  must therefore be 14 bytes and must end in `0x00`.

The original authors recorded why the payload repeats itself, in the comment at L36-38: a
repeated `hello` "stresses the compression code better" than the more conventional
non-repeating phrase would. That is the point of the fixture — 14 bytes containing a repeat
is enough to produce a real length/distance pair rather than literals alone.

### `minimal/dictionary.bin` — exactly 6 bytes

Declared at [`test/example.c`](../../../test/example.c) L40:

```c
static const char dictionary[] = "hello";
```

- **Content:** the 5 characters `hello` followed by one NUL terminator.
- **Length: 6 bytes, not 5.** Both dictionary calls in the C suite pass
  `(int)sizeof(dictionary)`, which for a 6-element `char` array is 6 — at L426 for
  `deflateSetDictionary` and at L477 for `inflateSetDictionary`. `dictionary.bin` must
  therefore be 6 bytes and must end in `0x00`.

**This is the single most likely error in this folder.** It matters because the dictionary
identifier a decompressor is asked to match is the Adler-32 checksum of the dictionary
bytes, and the two candidate lengths produce different checksums:

| Dictionary bytes | Length | Adler-32 | Correct? |
| --- | --- | --- | --- |
| `hello\0` | 6 | `0x08410215` | **Yes** — this is the contract |
| `hello` | 5 | `0x062c0215` | No — an off-by-one |

`0x08410215` is therefore a self-check constant for this fixture. The C suite captures it at
L429 into the `dictId` variable declared at L41 (`static uLong dictId;`, commented there as
the Adler-32 value of the dictionary) and asserts it at L472 when `inflate` reports that a
dictionary is needed. Reproduce it with:

```sh
python3 -c "import zlib; print('0x%08x' % zlib.adler32(open('minimal/dictionary.bin','rb').read()))"
# expected: 0x08410215
```

Note that Python's `hex()` would print this as `0x8410215`, dropping the leading zero; the
value is the same 32-bit quantity. Format with `%08x` to compare against the constant above.

## Licensing

**The fixtures in `minimal/` are all covered by this project's own licence** — see
[`LICENSE`](../../../LICENSE), "(C) 1995-2026 Jean-loup Gailly and Mark Adler". Those terms
are permissive: the software is provided as-is with no warranty, and anyone may use it for
any purpose including commercial use, and alter and redistribute it freely, subject to three
conditions — do not misrepresent the origin, mark altered versions plainly as altered, and
do not remove the notice from a source distribution. Read
[`LICENSE`](../../../LICENSE) for the operative wording; the summary here is not a
substitute for it.

This applies in two ways:

- `hello.bin` and `dictionary.bin` are **derived from
  [`test/example.c`](../../../test/example.c)**, an existing file of this project, and carry
  its terms unchanged.
- `empty.bin`, `single_byte.bin`, `repetitive.bin`, `random.bin`, `text.txt`, `binary.bin`,
  `gray_list.bin` and `window_boundary.bin` are **synthetic, authored for this repository**,
  and are contributed under the same terms. None is copied from a third party and none
  carries a separate obligation.

**Silesia is third-party data and is deliberately not redistributed here.** That is a
licensing reason for keeping tier 2 opt-in, additional to the CI reason: committing a
third-party corpus would place data of separate provenance under this repository's tree and
would require carrying its terms alongside our own. Instead `fetch_silesia.sh` retrieves it
on demand into an untracked directory, and whoever runs the script accepts the corpus's own
upstream terms. Those terms are not restated here, because the copy that matters is the one
upstream publishes at fetch time.

## Determinism is a requirement, not a preference

**Every fixture is a committed, deterministic byte sequence. Nothing in this folder is
generated at test time.** The tests read bytes from disk; they do not synthesise input, do
not seed a random number generator, and do not depend on the clock, the locale, the
filesystem or the host architecture. Two runs on two machines read identical bytes.

`random.bin` is the specific trap. Its *content* is arbitrary — that is what makes it
incompressible and therefore useful — but the file is a **fixed blob committed once**. The
tempting shortcut, filling a buffer with random bytes when the test starts, must not be
taken, not even with a hard-coded seed:

- A byte-identity failure has to be reproducible from the repository alone. If the input
  differs per run, a failing case cannot be re-run, bisected, or attached to a bug report.
- A "pass" would stop meaning anything fixed. The gate would assert that the two
  implementations agree on *whatever bytes today's run happened to produce*, which is a
  weaker claim than the one being made, and weaker in a way nobody would notice.
- Even a seeded generator is not portable: the byte stream is a property of the generator
  implementation, not of the seed.

This is why the ignore rules protect this folder explicitly.
[`.gitignore`](../../../.gitignore) scopes the fuzzer's `corpus/` exclusion to `fuzz/` on
purpose, and says so in a comment there, precisely so that an unanchored `corpus/` rule
cannot swallow this directory. Do not add such a rule.

Fixtures are part of the acceptance contract. Adding one widens the differential matrix's
coverage; altering or removing one narrows it, silently, because a matrix that no longer
covers a case still reports success.

## Filename constraints

Fixture names must use `.bin`, `.txt`, or no extension at all. A name must **never** end in
any of these:

`.o` · `.a` · `.lo` · `.dylib` · `.diff` · `.patch` · `.orig` · `.rej` · `~` · `.gcda` ·
`.gcno` · `.gcov`

The root [`.gitignore`](../../../.gitignore) carries all twelve of those as *unanchored*
patterns, so they match at any depth — including inside this folder. A fixture named
`binary.o` or `archive.a` would be silently untracked: it would work perfectly on the
machine that created it, and every correctness gate that reads it would fail on a fresh
clone with a file-not-found error that points at the test rather than at the missing commit.
The same hazard applies to `**/libz.so*`, so no fixture may be named `libz.so`-anything
either.

Check any new name before committing it. No output means the name is safe:

```sh
git check-ignore -v crates/zlib-rs-differential/corpus/minimal/<new-name>
```

All ten fixtures in the inventory above have been checked against the current ignore rules
and none is matched.

## The Silesia tier: path contract

Silesia is used **only** for performance measurement, by the `benches/deflate_bench.rs` and
`benches/inflate_bench.rs` suites at the repository root. Those suites are attached to this
crate through the `[[bench]]` entries in [`../Cargo.toml`](../Cargo.toml).

### Resolution order — one canonical rule

The corpus directory is located by exactly two steps, in this order:

1. `$ZLIB_RS_SILESIA_DIR`, if that environment variable is set and non-empty.
2. Otherwise `<repo-root>/target/silesia`.

`fetch_silesia.sh` writes only to that location, and the benchmarks probe only that
location. The two must never drift apart; if the resolution rule ever changes, it changes
here first and in both consumers in the same commit.

The default sits under `target/` deliberately. The root
[`.gitignore`](../../../.gitignore) ignores `/target/`, so a fetched corpus can never be
committed by accident. A default inside this folder — `corpus/silesia/` — would **not** be
ignored, and several hundred megabytes of third-party data would be one `git add -A` away
from the history. That is why the script must not default here.

### Absence is normal, not an error

**Benchmarks must skip gracefully when the directory is missing.** A developer running
`cargo bench` without having fetched anything should get a clear "Silesia not present,
skipping" message and a zero exit status — not an error, and under no circumstances an
attempt to download. Correctness is measured on tier 1 and does not depend on tier 2 in any
way.

### Opt-in only — the no-network rule

**`fetch_silesia.sh` is invoked by a human, deliberately, and by nothing else.**

- It is **never** invoked by `cargo test`.
- It is **never** invoked by CI.
- It is referenced from no `build.rs`, from no file under `tests/`, and from no workflow in
  `.github/workflows/`.

Those are not aspirations; they are the property that keeps the test suite hermetic. The
moment any automated path calls this script, `cargo test` acquires a network dependency and
every gate becomes contingent on a remote host. If you are adding automation and find
yourself wanting the corpus, the answer is to make the automation skip, exactly as the
benchmarks do.

The script needs nothing beyond the shell utilities present on any developer or CI machine.
It adds no crate to the workspace and no entry to any manifest.

### Manual invocation

```sh
# Default location: <repo-root>/target/silesia (git-ignored)
./crates/zlib-rs-differential/corpus/fetch_silesia.sh

# Or choose your own directory, e.g. a shared cache outside the repository
ZLIB_RS_SILESIA_DIR=/var/cache/silesia \
    ./crates/zlib-rs-differential/corpus/fetch_silesia.sh

# Then measure. The same variable is read by the benchmarks.
ZLIB_RS_SILESIA_DIR=/var/cache/silesia \
    cargo bench -p zlib-rs-differential
```

## How to add a fixture

Adding a fixture is a contract change. Do all five steps in one commit.

1. **Choose a safe filename.** `.bin`, `.txt` or no extension, and verify it with
   `git check-ignore -v` as shown above. No output means safe.
2. **Commit the bytes themselves.** Generate the content however you like — then commit the
   result and never regenerate it. Do not add a generator that runs at test time.
3. **Add a row to the inventory table** in this file, stating the exact size if the size is
   part of what the fixture tests, what code the fixture reaches, and the reference that
   justifies it. A fixture whose rationale cannot be written down is a fixture that is not
   needed.
4. **Update every consumer.** The tests name fixtures individually, so a new file is read by
   nobody until it is added to `tests/byte_identical.rs` and, where round-tripping is
   relevant, `tests/roundtrip_interop.rs`.
5. **Account for the cost.** Every fixture is multiplied by the whole differential matrix,
   which per AAP §0.6.4.4 spans compression levels 0-9, `windowBits` for all three container
   formats (raw, zlib and gzip), `memLevel` 1-9, five strategies, six flush modes, and both
   single-shot and incremental chunked feeding. Coverage is the reason to add a fixture;
   runtime is the reason to add only fixtures that reach something new.

Removing or renaming a fixture follows the same discipline in reverse, and narrows coverage
rather than widening it — say so explicitly in the commit message so the reduction is a
decision on the record rather than a side effect.

## Reference index

Every file below is **REFERENCE — read, never edited**. They are the authority for the
claims in this README; where this document and one of them disagree, they are right and this
document must be corrected.

| Reference | What it supplies |
| --- | --- |
| [`test/example.c`](../../../test/example.c) | The `hello` and `dictionary` literals (L35, L40), their lengths (`strlen(hello)+1`, `sizeof(dictionary)`) and the `dictId` check (L41, L429, L472) |
| [`doc/algorithm.txt`](../../../doc/algorithm.txt) | The LZ77 design: 32K distance limit, 258-byte length limit, length-3 hashing, most-recent-first chain order, level-driven chain truncation, and no lazy matching at levels 1-3 |
| [`doc/txtvsbin.txt`](../../../doc/txtvsbin.txt) | The allow / gray / block byte categories and the text-versus-binary rule, including the empty-input boundary case |
| [`doc/rfc1950.txt`](../../../doc/rfc1950.txt) | ZLIB Compressed Data Format Specification version 3.3 — the zlib container the byte-identity matrix covers |
| [`doc/rfc1951.txt`](../../../doc/rfc1951.txt) | DEFLATE Compressed Data Format Specification version 1.3 — block types, including the stored block `random.bin` provokes |
| [`doc/rfc1952.txt`](../../../doc/rfc1952.txt) | GZIP file format specification version 4.3 — the gzip container the matrix covers |
| [`trees.c`](../../../trees.c) | `build_tree`'s forced-two-codes fixup (L650-661), `detect_data_type` and its `0xf3ffc07f` block mask (L966-991), and the stored-block decision (L1027-1048) |
| [`LICENSE`](../../../LICENSE) | The terms every fixture in `minimal/` is contributed under |
