# Differential test corpus

This folder holds the input data for every correctness gate that **reads input bytes**, plus
the pointer to the optional large-input tier the repository-root benchmark suites measure
against. It is pure data, one shell script and this README — there is no Rust here,
nothing in this folder is compiled, and nothing in it participates in a build. The
differential harness that consumes it lives one directory up
([`../Cargo.toml`](../Cargo.toml)); this folder only supplies the bytes that harness feeds
to the Rust port and to the in-tree C oracle so their outputs can be compared.

Which gates those are is worth stating precisely, because "every correctness gate" would be
wrong in two directions. `tests/table_equality.rs` is a correctness gate that takes no input
sample at all, and the five `fuzz/` targets are correctness and security gates that generate
their own arbitrary bytes and read nothing from here — see [Which gates read this folder, and
which do not](#which-gates-read-this-folder-and-which-do-not) below.

**Every consumer named in this document exists and runs.** There are six — three tests and
three benchmarks — and only two of them ever look at tier 2:

| Consumer | Tier it reads | What it does with the bytes |
| --- | --- | --- |
| `tests/byte_identical.rs` | 1 only | Every fixture, multiplied by the configuration matrix |
| `tests/roundtrip_interop.rs` | 1 only | Every fixture, compressed by one implementation and inflated by the other |
| `tests/table_equality.rs` | neither | Reads no input sample at all — see below |
| `benches/deflate_bench.rs` | 1, and 2 when present | Tier-1 fixtures in every gated group; the `deflate_silesia` group over tier 2 |
| `benches/inflate_bench.rs` | 1, and 2 when present | The same, with `inflate_silesia` over tier 2 |
| `benches/checksum_bench.rs` | 1 only | A synthetic length sweep as its primary axis, plus the named tier-1 fixtures as secondary cases; it never reaches for Silesia |

All three benches belong to [`../../../benches/Cargo.toml`](../../../benches/Cargo.toml), the
excluded package `zlib-rs-benches`, which depends on this crate by path and reaches the fixtures
through it. Nothing is attached to this crate by a `[[bench]]` entry, and it takes no criterion
dependency; `cargo bench --manifest-path benches/Cargo.toml` is what builds and runs them. The
tier-2 path contract below is therefore a description of what the two Silesia consumers **do**,
verified by running them, rather than a specification something might later be written against.

Because the governing acceptance criterion is that compressed output be *byte-identical* to
the C reference, this corpus defines the sample over which that claim is measured. The
filenames, the pinned sizes, the byte-content classes and the determinism rule below are a
contract, and every consumer is written to conform to it.

```
corpus/
├── README.md            this file — the published contract
├── fetch_silesia.sh     opt-in; fetches only when a human runs it, never from CI
└── minimal/             the ten committed fixtures
```

## The two tiers

| | Tier 1 — `minimal/` | Tier 2 — Silesia |
| --- | --- | --- |
| **Committed to git?** | Yes, every byte | No, never |
| **How obtained** | Already here | `fetch_silesia.sh`, run by hand |
| **Used by** | Correctness gates that read input bytes | Benchmarks only |
| **Network needed** | Never | Yes, at fetch time only |
| **Required for `cargo test`** | Yes | No — absence is not a failure |

**Tier 1 is the sole input to every correctness gate that takes one.** The byte-identity
matrix and the round-trip interoperability tests read `minimal/` and nothing else. That is a
hard requirement rather than a convenience: `cargo test` and CI must never touch the network,
so no correctness gate may depend on data that has to be downloaded. A gate that reached for
Silesia would be a gate that silently passes on a machine with no network — or worse, one
whose result depends on what a remote server served that day.

### Which gates read this folder, and which do not

| Gate | Reads `minimal/`? | Its input |
| --- | --- | --- |
| `tests/byte_identical.rs` | **Yes** | Every fixture, multiplied by the whole configuration matrix |
| `tests/roundtrip_interop.rs` | **Yes** | Every fixture, compressed by one implementation and inflated by the other |
| `benches/deflate_bench.rs`, `benches/inflate_bench.rs` | **Yes** | Named fixtures as the tier-1 throughput and memory cases, plus the optional Silesia tier |
| `benches/checksum_bench.rs` | **Yes, secondarily** | Buffers it generates itself for the length sweep that is its main axis — a checksum's rate depends on length, not on what the bytes mean — and the named tier-1 fixtures for its `adler32_fixtures` and `crc32_fixtures` groups. A missing fixture logs once and is skipped. Never Silesia |
| `tests/table_equality.rs` | **No** | The generated C headers `crc32.h`, `trees.h` and `inffixed.h`, compared element for element against the ported Rust `const` arrays |
| `fuzz/fuzz_targets/*.rs` (all five) | **No** | Bytes the fuzzer generates, guided by coverage; libFuzzer maintains its own corpus under `fuzz/corpus/` |

Two rows deserve naming explicitly, because it is easy to assume otherwise.
`tests/table_equality.rs` compresses nothing and decompresses nothing: it asserts that the
transcribed Rust tables equal the committed C arrays, per AAP §0.4.1.4. It therefore takes no
sample, is unaffected by anything in `minimal/`, and adding or removing a fixture cannot
change its result. The five fuzz targets are likewise not consumers: their whole point is
input nobody chose, so a fixture added here neither widens nor narrows what they explore.

The three benchmarks read `minimal/` as well, and the fixture contract binds them for the
same reason — but they are not gates, so they are listed separately in the consumer table
above rather than here. A benchmark that cannot find a fixture reports it and carries on with
the rest; a gate that cannot find one fails.

This corpus adds no tooling and no crate to the workspace. `fetch_silesia.sh` relies only on
shell utilities, each of which it probes for by name rather than assuming — the concrete list
is under [Prerequisites](#prerequisites), which is the one place that contract is stated —
and the fixtures are read with ordinary file I/O. No dependency exists, or may be added, to
serve this folder.

## Fixture inventory — the contract

The tests are to load these files **by exact filename**. There is to be no globbing, no
directory scan and no fallback: a name that does not match is a missing file, not a skipped
case.
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

"small" means the exact length is not pinned — only the content class is. The four pinned
sizes (0, 1, 14 and 6 bytes) and the one pinned floor (> 32768 bytes) are load-bearing.

`hello.bin` and `dictionary.bin` are transcriptions of the `hello` and `dictionary` literals
of [`test/example.c`](../../../test/example.c) (L35 and L40), including `hello`'s trailing
NUL, so that the harness feeds the *same* bytes the existing C suite has always fed and a
disagreement is attributable to the implementation rather than to the input.

## The byte-classification categories

`text.txt`, `binary.bin` and `gray_list.bin` cover one category each of the partition
`detect_data_type` imposes on all 256 byte values ([`trees.c`](../../../trees.c) L966-991,
described in [`doc/txtvsbin.txt`](../../../doc/txtvsbin.txt) L41-46).

| Category | Bytes (decimal) | Effect on classification |
| --- | --- | --- |
| Allow list | 9, 10, 13, 32-255 | One or more, with no block-listed byte, yields `Z_TEXT` |
| Gray list | 7, 8, 11, 12, 26, 27 | Ignored — neither forces nor prevents either verdict |
| Block list | 0-6, 14-25, 28-31 | Any one yields `Z_BINARY` immediately |

**The block-list row follows the implementation, which differs from the prose document by
exactly two bytes — do not "correct" it.** `doc/txtvsbin.txt` L45-46 states the block list as
0-6 and 14-31, which would include 26 (SUB) and 27 (ESC); the mask `0xf3ffc07f` at
[`trees.c`](../../../trees.c) L971 leaves bits 26 and 27 clear, so SUB and ESC are tolerated,
consistent with that document's *own* gray list at L43-44. This is load-bearing for the
corpus: it is what makes `gray_list.bin` reach the fall-through, since a fixture containing
SUB or ESC would otherwise return early from the block-list loop and leave the third exit
unreachable. Widening the mask changes emitted `data_type` values and makes that fixture dead
weight.

## Provenance of the two `test/example.c` literals

Both lengths are off-by-one traps, so they are pinned here precisely.

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

The length matters because the dictionary identifier a decompressor is asked to match is the
Adler-32 checksum of the dictionary bytes, and the two candidate lengths produce different
checksums:

| Dictionary bytes | Length | Adler-32 | Correct? |
| --- | --- | --- | --- |
| `hello\0` | 6 | `0x08410215` | **Yes** — this is the contract |
| `hello` | 5 | `0x062c0215` | No — an off-by-one |

`0x08410215` is therefore a self-check constant for this fixture. The C suite captures it at
L429 into the `dictId` variable declared at L41 (`static uLong dictId;`, commented there as
the Adler-32 value of the dictionary) and asserts it at L472 when `inflate` reports that a
dictionary is needed. Reproduce it with:

Run this from the repository root, so the path does not depend on the current directory:

```sh
python3 -c "import zlib, sys; print('0x%08x' % zlib.adler32(open(sys.argv[1],'rb').read()))" \
  crates/zlib-rs-differential/corpus/minimal/dictionary.bin
# expected: 0x08410215
```

Note that Python's `hex()` would print this as `0x8410215`, dropping the leading zero; the
value is the same 32-bit quantity. Format with `%08x` to compare against the constant above.

## Licensing

The fixtures in `minimal/` are all covered by this project's own licence — see
[`LICENSE`](../../../LICENSE) for the operative wording. `hello.bin` and `dictionary.bin` are
derived from [`test/example.c`](../../../test/example.c), an existing file of this project,
and carry its terms unchanged; the other eight are synthetic, authored for this repository
and contributed under the same terms. None is copied from a third party.

**Silesia is third-party data and is deliberately not redistributed here.** That is a
licensing reason for keeping tier 2 opt-in, additional to the CI reason: committing a
third-party corpus would place data of separate provenance under this repository's tree and
would require carrying its terms alongside our own. Instead `fetch_silesia.sh` retrieves it
on demand into an untracked directory.

It is important to be precise about what "the corpus's terms" means here, because there is
no single set of them. **Silesia is not one work under one licence.** It is a collection of
twelve unrelated files drawn from twelve different sources, and upstream publishes no unified
licence over the collection. The expected inventory, and the origin each member is described
as having, is:

| Member | Described origin |
| --- | --- |
| `dickens` | Collected works of Charles Dickens — English prose |
| `mozilla` | Tarred Mozilla 1.0 executables — third-party binaries |
| `mr` | Medical magnetic-resonance image |
| `nci` | Chemical structure database |
| `ooffice` | An OpenOffice.org 1.01 shared library — third-party binary |
| `osdb` | Sample database in MySQL format |
| `reymont` | A PDF of Władysław Reymont's *Chłopi* |
| `sao` | The SAO star catalogue — binary records |
| `samba` | Tarred Samba source tree |
| `webster` | The 1913 Webster Unabridged Dictionary |
| `x-ray` | X-ray medical image |
| `xml` | Collected XML files |

Those origins carry materially different terms — public-domain and Project-Gutenberg texts,
GPL-licensed source, proprietary binaries, catalogues and images — and this repository is not
in a position to grant, restate or summarise any of them. **Whoever runs the script is
responsible for the terms of every member they thereby obtain.** That is why nothing here is
phrased as a licence grant, and why the corpus is confined to local throughput measurement
and is never redistributed or shipped.

The inventory is not merely documentation. `fetch_silesia.sh` enforces it in all three places
it forms an opinion about a directory: after extraction, so an archive that is not this corpus
is rejected rather than installed; under `--verify-only`, so a destination provisioned by an
earlier run is reported incomplete rather than blessed; and in the idempotent short-circuit
that skips a fetch, so "there is nothing to do" cannot be said about a partial corpus. All
three compare the payload's file names against exactly that twelve-name list
(`ZLIB_RS_SILESIA_MEMBERS`) and fail if anything is missing or anything extra is present.
One definition of complete, three callers.

Setting the variable to `-` skips the check, and the script warns about it **once, at the
entry point, whatever mode it was asked for**. That placement is deliberate rather than
incidental: two of those three callers — `--verify-only` and the idempotent short-circuit —
decide through a quiet predicate that returns "matches" without a word when the check is
opted out, so a warning emitted at the point of the check would have been silent on exactly
the runs that report success having examined no inventory at all.

The names are not the bytes, though, and the same three places also hash the members when a
per-member manifest is available — required under `--verify-only`, where it is the only
evidence that comes from outside the directory being judged. See
[Integrity](#integrity-and-the-difference-between-corruption-and-authentication).

## Determinism is a requirement, not a preference

**Every fixture is a committed, deterministic byte sequence. Nothing here is generated at
test time.** The tests read bytes from disk; they do not synthesise input, seed a random
number generator, or depend on the clock, locale, filesystem or host architecture.

`random.bin` is the specific trap. Its content is arbitrary — that is what makes it
incompressible and useful — but the file is a fixed blob committed once. Filling a buffer
with random bytes at test start must not be done, not even with a hard-coded seed: a failure
has to be reproducible from the repository alone, a "pass" over whatever bytes today's run
produced is a weaker claim than the one being made, and a seeded generator's byte stream is a
property of the generator rather than of the seed.

[`.gitignore`](../../../.gitignore) scopes the fuzzer's `corpus/` exclusion to `fuzz/` on
purpose, so that an unanchored `corpus/` rule cannot swallow this directory. Do not add one.

## Filename constraints

Fixture names must use `.bin`, `.txt`, or no extension. A name must **never** end in `.o`,
`.a`, `.lo`, `.dylib`, `.diff`, `.patch`, `.orig`, `.rej`, `~`, `.gcda`, `.gcno` or `.gcov`,
and must not be `libz.so`-anything: the root [`.gitignore`](../../../.gitignore) carries all
of those as *unanchored* patterns, so such a fixture would be silently untracked — working on
the machine that created it and failing on a fresh clone with a file-not-found error that
points at the test rather than at the missing commit.

Check any new name before committing it. No output means the name is safe:

```sh
git check-ignore -v crates/zlib-rs-differential/corpus/minimal/<new-name>
```

All ten fixtures above are checked against the current rules and none is matched.

## The Silesia tier: path contract

Silesia is to be used **only** for performance measurement, by
[`benches/deflate_bench.rs`](../../../benches/deflate_bench.rs) and
[`benches/inflate_bench.rs`](../../../benches/inflate_bench.rs) at the repository root, which
are hosted by the `[[bench]]` entries of [`benches/Cargo.toml`](../../../benches/Cargo.toml)
— an excluded package that depends on this crate, so that criterion's own MSRV cannot raise
the workspace's. Both read the corpus through the contract below and neither downloads
anything.

### Resolution order — one canonical rule

The corpus directory is located by exactly two steps, in this order:

1. `$ZLIB_RS_SILESIA_DIR`, if set and non-empty.
2. Otherwise `<repo-root>/target/silesia`.

`fetch_silesia.sh` writes only there and the benchmarks probe only there; if the rule
changes it changes here first and in both consumers in the same commit. The default sits
under `target/` because [`.gitignore`](../../../.gitignore) ignores `/target/`, so a fetched
corpus can never be committed by accident — a default of `corpus/silesia/` would **not** be
ignored and would put several hundred megabytes of third-party data one `git add -A` away
from the history.

### Two kinds of run, and only one of them may skip

Absence is normal for a **developer's** run and unacceptable for a **deciding** one, and the
benches distinguish the two explicitly rather than leaving it to whoever reads the log.
`ZLIB_RS_SILESIA_REQUIRED=1` selects the second:

| | Exploratory (the default) | Required (`ZLIB_RS_SILESIA_REQUIRED=1`) |
| --- | --- | --- |
| Corpus absent | one note naming this script; the tier-2 groups publish `expected=0 cases=0` and the run exits zero | the bench **panics** and the run fails |
| Some of the twelve present | measured over those, `verdict=incomplete` | refused: a number measured over part of the corpus is not the AAP §0.8.4 number |
| Some other directory's files | measured over the first twelve, sorted, `verdict=salvaged` and a note that it is not the pinned inventory | refused outright |
| Identity evidence | one `SILESIA verdict=<v> found=<n> of=12 required=no dir=<path>` line per suite | the same line with `required=yes`, and `verdict` must be `complete` |
| Downloads | none | none |

**Neither mode downloads anything, ever.** Required mode does not fetch the corpus; it
*requires that the corpus is already there*. The `bench` job of
[`../../../.github/workflows/rust.yml`](../../../.github/workflows/rust.yml) arms it only when
the repository variable `ZLIB_RS_SILESIA_SHA256` pins a digest, and even then the corpus has
to have been provisioned **from outside the workflow** — by a cache entry keyed on that
digest, or by a self-hosted runner that already holds it and exports `ZLIB_RS_SILESIA_DIR`.
The workflow's only invocation of `fetch_silesia.sh` is `--verify-only`, which fetches nothing
and writes nothing; when the pin is set and no corpus turns up, the job **fails** rather than
downloading one, and when the pin is unset the job says so with a `::warning::` instead of
letting a minimal-corpus run stand in silently for the AAP §0.8.4 Silesia measurement.

That the job proves its armed mode really refuses is not left to inspection either: every run,
armed or not, executes a network-free negative test that arms the benches against an empty
directory and requires them to fail.

So: a developer running `cargo bench` without having fetched anything gets a clear "Silesia
not present" message and a zero exit status. Correctness is measured on tier 1 and does not
depend on tier 2 in any way. What tier 2 decides is throughput, and that decision is only
taken by a run that has been armed.

### The two measurement profiles

A throughput ratio is only a fact about two implementations if both were built comparably,
and here they are not by default: the C oracle is compiled one translation unit at a time
with no cross-unit optimisation, while `--profile bench` gives the port fat LTO across the
whole workspace. It cannot be equalised on the C side — `objcopy --redefine-syms`, which
gives the oracle its `c_` prefix, cannot rename symbols inside the `.gnu.lto_*` IR an
`-flto` object carries, so an LTO oracle would rebind to the port's own symbols — so it is
equalised on the Rust side. Measure twice:

```sh
ZLIB_RS_BENCH_RUST_PROFILE=bench \
    cargo bench --locked --manifest-path benches/Cargo.toml --profile bench
ZLIB_RS_BENCH_RUST_PROFILE=bench-parity \
    cargo bench --locked --manifest-path benches/Cargo.toml --profile bench-parity
```

`bench-parity` is `bench` with LTO off and codegen units uncollapsed. Every summary line
carries `c_profile=` and `rust_profile=`, the profile name is baked into the binary at build
time (cargo does not expose it to a build script, so it is declared), and the CI gate decides
each case on the **stronger — worse-for-the-port —** of the two passes. It is not a
formality: two cases pass under fat LTO and fail without it.

### Opt-in only — the no-network rule

**Fetching is invoked by a human, deliberately, and by nothing else. Verifying is what CI does
with this script, in two read-only modes, and it obtains nothing.**

That distinction is the whole of this section, and an earlier version of this file opened by
saying the script was "invoked by a human and by nothing else" and then, three lines later,
documented the workflow's own invocation of it. Both halves were true and together they read as
a contradiction. Stated precisely, there are exactly three modes and CI uses two of them:

| mode | reaches the network | writes anything | invoked by CI |
|---|---|---|---|
| *(no flag)* — fetch | **yes** | yes, the corpus | **never** |
| `--verify-only` | no | no | yes, conditionally |
| `--pin-status` | no | no | yes, unconditionally |

- **CI never fetches.** No workflow step invokes this script without one of the two read-only
  flags, and there is no code path in either of those two that downloads, extracts or creates
  anything. That is what keeps `cargo test` hermetic.
- **`--pin-status`** runs unconditionally in the `bench` job. It prints what `silesia.pin`
  records and exits, touching no destination, no network and no tool — its whole purpose is to
  tell the workflow whether this repository carries an approved corpus identity at all, so that
  "the acceptance measurement was not taken" can be distinguished from "it cannot be taken
  here".
- **`--verify-only`** runs when a corpus has been provisioned into the job's cache. It confirms
  that the corpus already on disk carries a well-formed stamp recording the URL and archive
  digest expected now, and, when `silesia.sha256` is present, that every member's digest
  matches. It fetches nothing and writes nothing.
- **The job fails closed on the designated release runner** when the pin is armed and the
  corpus is nevertheless absent. On a hosted runner the same condition is a notice, because a
  hosted runner's throughput numbers are informational anyway — see
  `.github/workflows/rust.yml`.
- It is invoked from no `build.rs` and from no file under `tests/`, in any mode.

Those are not aspirations; they are the property that keeps the test suite hermetic. The
moment any automated path calls this script **in fetching mode**, `cargo test` acquires a
network dependency and every gate becomes contingent on a remote host. If you are adding
automation and find yourself wanting the corpus, the answer is to make the automation skip,
exactly as the benchmarks do — or, if it must know whether an already-provisioned corpus is
the pinned one, to ask with `--verify-only`, or whether one could be, with `--pin-status`.

### What is pinned, and where

`silesia.pin` and `silesia.sha256` are committed beside the script and are the repository's
record of which corpus is approved. They exist because the alternative was worse: the script
used to ship its built-in digest as the literal string `UNPINNED`, so verification depended on
an environment variable somebody had to remember to set, and a required CI check could be green
having verified nothing at all.

- **`silesia.pin`** — the archive's URL, its SHA-256, and the four resource bounds the fetch
  enforces *before* it writes 65 MiB and *before* it expands that into 202 MiB. Every bound is
  the measured truth about the approved archive, applied with a stated slack factor, so a
  hostile archive is refused on its declared size rather than only on its digest, and refused
  early. Run `./fetch_silesia.sh --pin-status` to read it.
- **`silesia.sha256`** — all twelve members' digests in `sha256sum` format, so
  `sha256sum --check` reads it directly. The script uses it automatically when no
  `ZLIB_RS_SILESIA_MEMBER_SHA256` is given. This survives repackaging where the archive digest
  does not, and it is what makes an *already-extracted* directory verifiable.

Both are trust-on-first-use with respect to upstream: they prove the corpus has not changed
since it was pinned here, not that what was pinned is what the Silesia authors published. For
the latter, obtain the digests through a channel independent of the download and pass them
explicitly. Change `url` and `sha256` together and never separately — a digest without the URL
it came from asserts nothing about the provenance of the bytes.

### Prerequisites

The script adds no crate to the workspace and no entry to any manifest, but it is **not**
dependency-free, and it probes for each of the following rather than assuming it — a minimal
container image frequently has none of the three groups:

| Needed for | Accepted programs |
| --- | --- |
| Download | `curl` **or** `wget` |
| Digest | `sha256sum` **or** `shasum` |
| Unpacking a zip | `unzip` **or** `bsdtar` |

GNU `tar` cannot read zip archives and is not a substitute for the third row. Beyond those,
the script uses only POSIX shell built-ins and the common coreutils/findutils it invokes
directly — `find`, `basename`, `sort`, `wc`, `tr`, `head`, `mv`, `mkdir`, `printf`, `rm`,
`date`, `cat`. A missing tool is reported by name with the alternatives that would satisfy
it, and the script exits without touching the network.

Budget roughly 65 MiB downloaded and a few hundred megabytes unpacked; allow about 300 MiB
free in the destination's filesystem.

### Integrity, and the difference between corruption and authentication

**A fetch without an expected digest is refused, and `silesia.pin` supplies one.** The check
cannot be waived — there is no flag to skip it and a mismatch is always fatal — but it no
longer depends on the caller remembering a value: a bare `./fetch_silesia.sh` reads the
committed pin described under *What is pinned, and where* above and proceeds. Remove or empty
that file and the refusal returns: with no pin, no `--sha256` and no
`ZLIB_RS_SILESIA_SHA256`, the script downloads nothing, exits non-zero and says so.

What the pin does **not** do is change what a digest is worth, and it would be easy to read a
committed file as though it did. A digest computed from the same download it is checking
detects **corruption** — a truncated transfer, a flaky proxy, a bit flip — and nothing more.
It cannot **authenticate** the bytes, because anyone positioned to alter the archive in flight
is equally positioned to alter the digest you would then compute from it. That is exactly what
`silesia.pin` holds: item 3 of the list below, trust-on-first-use, stated as such here and in
the script's own `CHECKSUM POLICY` block rather than left to be inferred. Its value is that the
approved corpus is now named in a reviewed file that CI can read, not that upstream has been
authenticated.

Authentication requires evidence arriving through a channel independent of the download. Both
checks this script offers can authenticate when their value arrives that way — what differs is
**what** each one authenticates, which is the distinction to hold on to:

1. **Per-member digests from an independent source** — a published paper, a distribution
   package, an existing trusted copy — supplied as a `sha256sum`-format manifest via
   `ZLIB_RS_SILESIA_MEMBER_SHA256`. Every listed member is hashed and must match: after
   extraction on a fetch, and against the installed directory under `--verify-only`. This
   authenticates the **extracted content**, independently of how the archive was packed, so it
   survives upstream rebuilding the zip and carries across mirrors. It is optional on a fetch,
   which has already hashed the archive it downloaded, and **required by `--verify-only`**,
   which downloads nothing — there, the manifest must cover every name in the inventory, and
   `ZLIB_RS_SILESIA_MEMBER_SHA256=-` is the only way to waive it, which the report then states
   in so many words. It defaults to the committed `silesia.sha256`.
2. **An archive digest obtained from such a source** and passed as `--sha256` or
   `ZLIB_RS_SILESIA_SHA256`; both outrank the pin, which is what makes an independently
   obtained value usable without editing a committed file. This authenticates **one exact
   archive**: match it and you hold precisely the bytes that value describes. What it cannot
   survive is repackaging — zip archives are not reproducible, so a rebuilt archive of the
   identical twelve files has a different digest and is indistinguishable from a hostile one.
   Strictly narrower than (1), and not weaker within its scope.
3. **An archive digest computed from your own download.** Corruption detection only — the same
   check with no independent evidence behind it, which is trust-on-first-use however the value
   is spelled. Acceptable for a throughput measurement on a machine you control, provided it
   is not mistaken for more than that. **This is the category `silesia.pin` falls in.**

So neither check is "the only one that authenticates", and neither bootstraps trust on its
own: (1) and (2) differ in scope, and both collapse into (3) if the value came from the
download's own channel.

Two further checks are independent of all of the above and always run, so they bound what an
unexpected archive can do even when its digest matched: the twelve-name inventory check
described earlier, and the extraction hardening — member paths are preflighted before
anything is written, rejecting absolute paths, `..` components and Windows drive/backslash
forms, and the extracted tree is then rejected if it contains anything that is not a regular
file or a directory, which excludes symlinks, devices, FIFOs and sockets.

#### How to supply the digest, and what re-use depends on

Supply it in whichever way suits you — `--sha256`, then `ZLIB_RS_SILESIA_SHA256`, then
`silesia.pin`, first match wins, and supplying nothing selects the pin. Change `ZLIB_RS_SILESIA_URL` and the
expected digest together: they identify one archive jointly. The `CHECKSUM POLICY` block at the
top of the script shows how to obtain and cross-check a value.

Two consequences worth knowing before you hit them:

- **A previous fetch is re-used only if it matches.** The script stamps the destination with the
  URL and digest it verified. On a later run both are compared against what you expect *now*,
  before it can report that there is nothing to do. A stamp left behind by a fetch from
  somewhere else, or one written by hand, is refused rather than believed — the stamp is
  evidence of provenance, never of authenticity. Pass `--force` to discard what is there and
  refetch, which verifies the fresh download exactly as any other.
- **`--verify-only` needs the digest too**, because it compares the stamp against what you
  expect now. Verifying against nothing is not verification, so there is no digest-free form of
  any invocation.
- **`--verify-only` needs a per-member manifest as well**, or an explicit
  `ZLIB_RS_SILESIA_MEMBER_SHA256=-` waiver. The reason is the same argument one level down: the
  stamp, the file names and the inventory all live *inside* the directory being vouched for, and
  the archive is not retained after a fetch, so its digest can never be recomputed there. Rewrite
  a member and every one of those checks still passes. A manifest is the one expectation that
  comes from elsewhere, so that mode hashes every member against it and refuses a manifest that
  covers only part of the inventory. Under the `-` waiver the closing report says that it
  establishes nothing about the bytes, rather than the word "Verified" standing alone.

### Extraction is not trusted either

A digest match says the archive is the one expected. It says nothing about
whether the archive is well behaved, and member names inside a zip are
attacker-controlled: they can be absolute, climb out with `..`, carry a Windows
drive letter or backslash separators, or name a symbolic link pointing anywhere
on the filesystem.

Neither `unzip` nor `bsdtar` can be relied on to stop that — measured against
purpose-built archives the two disagree on every hostile case, one silently
rewriting what the other rejects, and *both* create an escaping symlink without
complaint. So the script checks for itself: every member name must be an
ordinary relative path, checked **before** anything is extracted, and the
unpacked tree must contain nothing but regular files and directories, checked
before anything is installed. A legitimate Silesia archive passes both; anything
else is refused with the offending member named, and the destination is never
created.

### What is checked before anything is installed

Everything the script consumes is either upstream-controlled or environment-controlled, so
each of the following is a check on input it does not trust. All four are unconditional —
there is no flag, and no environment variable, that waives any of them.

- **The URL, and every hop it leads through.** `ZLIB_RS_SILESIA_URL` must begin with
  `https://` and contain no whitespace or control character. That is the whole accepted set:
  `http://`, `file://` and every other scheme are refused, before anything is downloaded or
  created, at the entry point of the script. It is additionally passed to `curl`/`wget` after
  a `--` operand terminator, so a value beginning with `-` could never be read as a downloader
  option even if the scheme check were somehow bypassed. Checking the value alone would not be
  enough, though: both tools follow redirects, so a `302` to `http://` would have moved the
  transfer somewhere the scheme check had already refused. The download therefore also passes
  `--proto '=https' --proto-redir '=https'` to `curl` and `--https-only` to `wget`, and the
  tool probe **selects only a downloader that accepts its option** — a build of either that
  does not is passed over rather than used unprotected. If you want to fetch from a mirror or
  from a copy you already have, serve it over HTTPS or install the corpus into the destination
  directory yourself and skip the script.
- **The bytes.** The download is verified against an expected SHA-256 before anything else
  reads it, and a mismatch is fatal. Upstream publishes no digest, so the in-script constant
  ships at the `UNPINNED` sentinel and the digest is a mandatory input you supply, with
  `--sha256` or `ZLIB_RS_SILESIA_SHA256`. Every invocation needs one, `--verify-only`
  included; a run that supplies neither is refused before it touches the network — see the
  CHECKSUM POLICY block at the top of the script.
- **The member paths.** The archive is listed and inspected *before* extraction; an absolute
  path, a `..` component, a symbolic link or a hard link causes it to be refused outright. A
  digest proves *which* archive arrived, not that its member paths are safe to write. Both
  supported extractors also sanitise such paths by default, but they do so *silently* and
  still exit zero, so that default is treated as a second layer rather than as the check.
- **The install.** Extraction goes into a staging directory beside the destination, and the
  destination only ever comes into existence as a completed rename, so a run that fails at
  any point installs nothing and leaves an existing corpus as it was.

### Manual invocation

Every invocation carries an expected digest, `--verify-only` included, and the committed
`silesia.pin` provides one — so the simplest form takes no digest at all. Pass one explicitly
only to override the pin: for a mirror, a repackaging, or a value you obtained independently
(see [Integrity](#integrity-and-the-difference-between-corruption-and-authentication) above for
what that does and does not establish). Replace `<sha256>` with the 64-hex digest you obtained.
`ZLIB_RS_SILESIA_URL` may be overridden, but only with an `https://` URL.

```sh
# Simplest form: the pinned digest and the pinned bounds, nothing to remember
./crates/zlib-rs-differential/corpus/fetch_silesia.sh

# Read what is pinned, and which ceilings it implies.  No network, no writes.
./crates/zlib-rs-differential/corpus/fetch_silesia.sh --pin-status

# Default location: <repo-root>/target/silesia (git-ignored)
ZLIB_RS_SILESIA_SHA256=<sha256> \
    ./crates/zlib-rs-differential/corpus/fetch_silesia.sh

# The digest may equally be passed as a flag; the two are interchangeable
./crates/zlib-rs-differential/corpus/fetch_silesia.sh --sha256 <sha256>

# Or choose your own directory, e.g. a shared cache outside the repository
ZLIB_RS_SILESIA_SHA256=<sha256> ZLIB_RS_SILESIA_DIR=/var/cache/silesia \
    ./crates/zlib-rs-differential/corpus/fetch_silesia.sh

# Strongest form: also verify independently obtained per-member digests
ZLIB_RS_SILESIA_SHA256=<sha256> \
    ZLIB_RS_SILESIA_MEMBER_SHA256=/path/to/silesia.sha256 \
    ./crates/zlib-rs-differential/corpus/fetch_silesia.sh

# Report on an existing download without fetching anything.  A digest applies here
# too -- --verify-only compares the recorded stamp against what is expected now --
# and it comes from the pin unless one is given, which is how CI runs this mode.
ZLIB_RS_SILESIA_DIR=/var/cache/silesia \
    ./crates/zlib-rs-differential/corpus/fetch_silesia.sh --verify-only

# Waiving the per-member manifest deliberately and visibly.  The report then says
# plainly that it checked the recorded fetch and the inventory and nothing about
# the bytes.
ZLIB_RS_SILESIA_DIR=/var/cache/silesia \
    ZLIB_RS_SILESIA_MEMBER_SHA256=- \
    ./crates/zlib-rs-differential/corpus/fetch_silesia.sh --verify-only

# Then measure. The same variable is read by the benchmarks.
ZLIB_RS_SILESIA_DIR=/var/cache/silesia \
    cargo bench --manifest-path benches/Cargo.toml
```

`./fetch_silesia.sh --help` prints the authoritative list of options and environment
variables; where it and this section disagree, the script is what actually runs.  It touches no network and creates no files.

## How to add a fixture

Adding a fixture is a contract change. Do all five steps in one commit.

1. **Choose a safe filename** — `.bin`, `.txt` or no extension — and verify it with
   `git check-ignore -v` as above.
2. **Commit the bytes themselves.** Generate the content however you like, then commit the
   result and never regenerate it. Do not add a generator that runs at test time.
3. **Add a row to the inventory table** in this file, stating the exact size if the size is
   part of what the fixture tests, what code the fixture reaches, and the reference that
   justifies it. A fixture whose rationale cannot be written down is a fixture that is not
   needed.
4. **Update every consumer.** The tests name fixtures individually, so a new file will be
   read by nobody until it is added to the `FIXTURES` table in `tests/byte_identical.rs` and
   in `tests/roundtrip_interop.rs`. This is not optional: both files load the whole table and
   **fail** when a name is missing or a pinned length has drifted, which is the consumer-side
   half of the breaking-change rule above. `tests/table_equality.rs` is never a consumer; it
   reads no fixtures at all.
5. **Account for the cost.** Every fixture is multiplied by the whole differential matrix,
   which per AAP §0.6.4.4 spans compression levels 0-9, `windowBits` for all three container
   formats (raw, zlib and gzip), `memLevel` 1-9, five strategies, seven flush values, and both
   single-shot and incremental chunked feeding. Seven rather than six: the six flushes a
   `deflate` call accepts — `Z_NO_FLUSH`, `Z_PARTIAL_FLUSH`, `Z_SYNC_FLUSH`, `Z_FULL_FLUSH`,
   `Z_FINISH` and `Z_BLOCK` — plus `Z_TREES`, which the tests pass deliberately in order to
   assert that BOTH implementations refuse it with the same status, since it is valid for
   `inflate` only and `deflate.c:985` answers `Z_STREAM_ERROR` for `flush > Z_BLOCK`. That
   seventh value is a status-parity case rather than a byte-identity one; `FLUSHES` in
   `tests/byte_identical.rs` is the authority. Coverage is the reason to add a fixture; runtime
   is the reason to add only fixtures that reach something new.

Removing or renaming a fixture narrows coverage rather than widening it. Say so explicitly in
the commit message, so the reduction is a decision on the record rather than a side effect.

## Reference index

Every file below is **REFERENCE — read, never edited**. They are the authority for the claims
above; where this document and one of them disagree, they are right.

| Reference | What it supplies |
| --- | --- |
| [`test/example.c`](../../../test/example.c) | The `hello` and `dictionary` literals (L35, L40), their lengths and the `dictId` check (L41, L429, L472) |
| [`doc/algorithm.txt`](../../../doc/algorithm.txt) | The LZ77 design: 32K distance limit, 258-byte length limit, length-3 hashing, most-recent-first chain order, level-driven chain truncation, and no lazy matching at levels 1-3 |
| [`doc/txtvsbin.txt`](../../../doc/txtvsbin.txt) | The allow / gray / block byte categories and the text-versus-binary rule, including the empty-input boundary case |
| [`doc/rfc1950.txt`](../../../doc/rfc1950.txt) | The zlib container the byte-identity matrix covers |
| [`doc/rfc1951.txt`](../../../doc/rfc1951.txt) | DEFLATE block types, including the stored block `random.bin` provokes |
| [`doc/rfc1952.txt`](../../../doc/rfc1952.txt) | The gzip container the matrix covers |
| [`trees.c`](../../../trees.c) | `build_tree`'s forced-two-codes fixup (L650-661), `detect_data_type` and its `0xf3ffc07f` block mask (L966-991), and the stored-block decision (L1027-1048) |
| [`LICENSE`](../../../LICENSE) | The terms every fixture in `minimal/` is contributed under |
