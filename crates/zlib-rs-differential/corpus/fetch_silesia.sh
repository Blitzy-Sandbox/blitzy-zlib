#!/bin/sh
# fetch_silesia.sh -- materialise the Silesia compression corpus so that the
#                     repository-root benchmark suites have realistic input.
#
# This is the only networked artifact anywhere in this crate, and the most
# important thing about it is what it does *not* do:
#
#   * it is NEVER invoked by `cargo test`;
#   * NO GATE EVER FETCHES through it -- see the exact boundary below;
#   * it is referenced from no `Cargo.toml`, from no `build.rs` and from no file
#     under `tests/`;
#   * it has no side effect of any kind -- not a directory, not a temporary
#     file, not a single byte of network traffic -- unless a human runs it with
#     NEITHER `--verify-only` NOR `--pin-status`.
#
# THE EXACT CI BOUNDARY, because a looser sentence used to stand here and was not
# true.  Exactly ONE job of `.github/workflows/rust.yml` names this script, and it
# never fetches:
#
#   * `bench`, which MEASURES, invokes it in exactly TWO modes, and neither of them
#     fetches or writes anything:
#
#       - `--pin-status`, unconditionally, on every runner.  It prints what
#         ./silesia.pin records and exits, reaching no network, no destination and
#         no unpacker.  What it is FOR is the workflow's arming decision: the pin, a
#         committed and reviewed file, is what says whether this repository carries
#         an approved corpus identity at all.  That used to be an optional
#         repository variable nobody had to set, which meant a required check could
#         go green having never measured the corpus AAP 0.8.4 names.
#       - `--verify-only`, when a corpus has been provisioned into the job's cache.
#         It "fetches nothing and writes nothing" (see `verify_only` below) and
#         therefore reaches no network and touches no path.
#
#     It NEVER invokes it in fetching mode, and it fails closed when the corpus it
#     was told to expect is absent rather than downloading it.  On a schedule or a
#     published release that absence is fatal, because AAP 0.8.4 states the
#     throughput bar against this corpus; on a hosted runner it is announced as a
#     warning, because a hosted runner's throughput verdict is informational anyway.
#
# ★ AND THERE IS NO SECOND CALLER.  There used to be a `silesia-provision` job here
# that invoked this script in FETCHING mode from a `workflow_dispatch` input, on the
# reasoning that it gated nothing and so stayed inside AAP 0.6.4.4.  It has been
# REMOVED, because that reasoning does not survive the text: AAP 0.6.4.4 and 0.3.1
# say this script is opt-in and "never invoked by `cargo test` or CI" -- a property
# of the WORKFLOW, not of whether the invoking job happens to gate something.  A
# fetching path in CI is a fetching path in CI.  The same file also claimed in its
# own header that "No job downloads a benchmark corpus", so the tree asserted both
# halves of a contradiction at once.
#
# Provisioning is therefore entirely off-workflow, which is what the AAP intends: a
# human runs this script by hand on a trusted machine, then either sets
# ZLIB_RS_SILESIA_DIR on a runner that holds the result or populates the
# `silesia-<digest>` cache the `bench` job restores.  The acceptance gate still
# REQUIRES the corpus on a schedule, on a release and on the designated runner --
# it fails closed and prints those instructions rather than downloading anything.
#
# So the property AAP 0.6.4.4 asks for is intact, and it is worth stating exactly:
# the corpus is opt-in "so `cargo test` and CI never require the network".  No gate
# requires a download, a side-effect-free verification does not require one, and
# NOTHING in any workflow downloads it at all.
#
# Those are not aspirations, they are the property that keeps this crate's
# test suite hermetic.  The correctness gates here must be reproducible from a
# clone alone, so `cargo test` must never touch the network.  Two of them read
# corpus bytes -- the byte-identity matrix and the round-trip interoperability
# tests -- and read them from `minimal/`; the table-equality checks read no
# corpus at all, since they compare this port's `const` tables against the
# generated C headers.  The moment any automated path calls this script IN
# FETCHING MODE, every gate becomes contingent on a remote host that may be
# unreachable, may be slow, or may serve something different today than it served
# yesterday.  If you are adding automation and find yourself wanting this corpus,
# the answer is to make the automation skip, exactly as the benchmarks do -- or,
# if it must know whether a corpus provisioned elsewhere is the pinned one, to ask
# with `--verify-only`, or whether one is pinned at all, with `--pin-status`.
# Those two are the modes that neither fetch nor write.  What it is NOT is to add a
# fetching caller of ANY kind, however carefully hedged -- not from a push, a pull
# request, a schedule, a release, and not from a hand-dispatched input either.  One
# was added once, guarded by every condition available, and it still made the tree
# contradict itself and the AAP.  A fetching call anywhere in a workflow makes the
# gates contingent on a remote host, which is the property these paragraphs exist to
# protect.  Provisioning belongs outside CI.
#
# WHY IT EXISTS
#
# Solely so that `cargo bench` has large, heterogeneous, realistic input.  The
# suites at the repository root -- benches/deflate_bench.rs and
# benches/inflate_bench.rs, hosted by the `[[bench]]` entries of
# benches/Cargo.toml, an excluded package that depends on this crate -- measure
# throughput against the in-tree C reference, and
# throughput only means something when it is measured on data of realistic size
# and variety.  Silesia is the standard corpus for exactly that.  This is
# tier 2 of the two-tier corpus described in AAP 0.6.4.4, and it is the
# resolution of AAP 0.8.2 ambiguity #5: corpus acquisition was left
# unspecified by the requirements, and a download performed at build time or
# test time would have been unacceptable in CI.
#
# PERFORMANCE ONLY.  NEVER AN INPUT TO A CORRECTNESS GATE.
#
# Nothing this script writes may ever back a correctness claim.  What it
# produces is not committed to this repository, so it is not deterministic
# across machines: two developers can hold different bytes under the same path
# and neither can prove from the repository which is right.  A byte-identity
# failure has to be reproducible from a clone, which is why every correctness
# fixture is instead a committed, deterministic blob under `minimal/`.
# Absence of this corpus is normal and is not an error -- the benchmarks report
# it and skip, with a zero exit status, and they never attempt a download.
#
# Silesia is also third-party data, and it is deliberately not redistributed by
# this repository.  It is not one work under one licence either: the twelve
# members listed under ZLIB_RS_SILESIA_MEMBERS below come from twelve different
# sources -- Project Gutenberg text, Mozilla and OpenOffice.org binaries, a MySQL
# sample database, a Samba source tree, medical images, a star catalogue -- each
# carrying its own terms, and upstream publishes no single unified licence over
# the collection.  Whoever runs this script is responsible for the terms of every
# member they thereby obtain, which is why nothing here is restated as though it
# were a licence grant, and why the corpus is confined to local throughput
# measurement and never redistributed or shipped.
#
# See ./README.md for the full rationale, the tier-1 fixture inventory, and
# the path contract this script implements.  Where this script and that README
# disagree about a path, the README is the published contract and this script
# is wrong.
#
# Every download is verified against an expected SHA-256, and that check cannot
# be waived: there is no flag to skip it, no environment variable that disables
# it, and a mismatch is always fatal.
#
# ★ THE DIGEST IS PINNED, IN ./silesia.pin, AND THE NEXT PARAGRAPH SAYS WHAT THAT
# IS WORTH.  This block used to say the opposite -- that SILESIA_SHA256_EXPECTED
# was left at the sentinel `UNPINNED` and every invocation had to supply a digest
# -- and that was true until the pin file arrived.  It is no longer: a bare
# `./fetch_silesia.sh` reads ./silesia.pin, enforces the digest, member count,
# per-member size, expanded size and expansion ratio it records, and downloads.
# The sentinel survives for the case the pin file is absent, where this script
# still FAILS CLOSED and refuses to fetch anything at all.
#
# What changed is WHO decides, not how strict the check is.  A digest that had to
# be remembered on every invocation made the AAP's own acceptance corpus depend on
# a person's memory, and it let a required CI check go green having verified
# nothing.  A committed, reviewed file decides instead -- and it is `grep`-able by
# the workflow, which is what lets CI tell an armed run from an unarmed one.
#
# ★ WHAT THE PIN IS AND IS NOT.  It is category 3 below: a digest computed from
# one download on one machine.  It detects CORRUPTION and it is trust-on-first-use;
# it is NOT authentication, and pinning it in a reviewed file does not make it so.
# Anyone who needs authentication must bring evidence from a channel independent of
# the download -- per-member digests through ZLIB_RS_SILESIA_MEMBER_SHA256, or an
# archive digest from an independent source through `--sha256`, both of which
# outrank the pin.  The pin is honest about its own strength; do not let its
# presence in a committed file be read as more.
#
# WHAT A DIGEST DOES AND DOES NOT ESTABLISH.  A digest computed from the same
# download it is checking against detects CORRUPTION -- a truncated transfer, a
# flaky proxy, a bit flip -- and nothing more.  It cannot AUTHENTICATE the bytes,
# because an attacker positioned to alter the archive is equally positioned to
# alter the digest you would compute from it.  Authentication requires evidence
# that arrives through a channel independent of the download.  In descending
# order of strength:
#
#   1. Per-member digests from an independent source -- a published paper, a
#      distribution package, a colleague's existing copy -- checked with
#      ZLIB_RS_SILESIA_MEMBER_SHA256.  These also survive upstream rebuilding
#      the zip, which an archive digest does not.
#   2. An archive digest obtained from such a source, passed with `--sha256` or
#      through ZLIB_RS_SILESIA_SHA256, or written into ./silesia.pin in a fork.
#   3. An archive digest computed from your own download: corruption detection
#      only.  Fine for a throughput measurement on a machine you control, and
#      it must not be mistaken for more than that.  **This is what ./silesia.pin
#      holds**, and ./README.md records the same caveat.
#
# The inventory check (ZLIB_RS_SILESIA_MEMBERS) and the extraction hardening in
# `unpack_archive` are independent of all of this and always run: they bound what
# an unexpected archive can do even when the digest matched.
#
# To obtain and use a digest, on a machine you trust:
#
#   1. Fetch the archive by hand:
#        curl -fSL -o silesia.zip -- \
#            'https://sun.aei.polsl.pl/~sdeor/corpus/silesia.zip'
#   2. Compute its digest:
#        sha256sum silesia.zip            # or: shasum -a 256 silesia.zip
#   3. Cross-check that value against an independent source if you can, then
#      either write it into ./silesia.pin -- one file, one line, visible in a
#      diff and reviewable -- or supply it on the invocation, which outranks the
#      pin:
#        ZLIB_RS_SILESIA_SHA256=<64-hex-digest> ./fetch_silesia.sh
#      Run `./fetch_silesia.sh --pin-status` to see what is pinned right now and
#      which ceilings that pin implies.
#
# -----------------------------------------------------------------------------
# WHAT IT COSTS AND WHERE IT LANDS
# -----------------------------------------------------------------------------
#
# The archive is roughly 65 MiB and expands to a few hundred megabytes, so
# allow around 300 MiB of free space in the destination's filesystem: the
# download is staged next to the destination so that the final install is a
# same-filesystem rename rather than a cross-device copy.
#
# The destination is resolved exactly as ./README.md specifies -- see
# `resolve_destination` below -- and defaults under `target/`, which the root
# .gitignore already ignores.  A default inside this folder would *not* be
# ignored, and several hundred megabytes of third-party data would then be one
# `git add -A` away from the history.  That is why this script never writes
# beside itself.
#
# Portability: POSIX shell only, matching the in-tree ./configure.  No bashisms,
# no `local`, no arrays, no `readlink -f`, no process substitution.  It adds no
# crate to the workspace and no entry to any manifest.
#
# PREREQUISITES, stated concretely rather than assumed present.  `probe_tools`
# checks for each of the first three and refuses to start without them, naming
# what to install; the last two are POSIX utilities this script uses directly.
#
#   downloader   curl (preferred) or wget
#   digest       sha256sum (GNU coreutils) or shasum (ships with macOS)
#   extractor    unzip (preferred) or bsdtar from libarchive
#                (libarchive-tools on Debian/Ubuntu).  GNU tar CANNOT read zip
#                archives and is deliberately not probed for.
#   find, basename, sort, wc, tr, head, mv, mkdir, printf -- POSIX utilities,
#                used by the extraction checks and the staging logic.
#
# A minimal container image frequently has none of the first three.

set -eu

# Configuration.  Every one of these is overridable from the environment; the
# `${VAR:-default}` form means an empty value is treated as unset, which is
# what makes `ZLIB_RS_SILESIA_DIR=` behave like "use the default" rather than
# like "install into the filesystem root".

# Where the corpus is installed.  Empty or unset selects the default computed
# in `resolve_destination`, which is <repo-root>/target/silesia.  This is the
# same variable the benchmarks read, so whatever you set here you must also
# set when you run `cargo bench`.
ZLIB_RS_SILESIA_DIR=${ZLIB_RS_SILESIA_DIR:-}

# Where the corpus comes from.  The Silesia corpus is published by Sebastian
# Deorowicz of the Silesian University of Technology, Gliwice; the landing
# page is https://sun.aei.polsl.pl/~sdeor/index.php?page=silesia and the
# archive is the single zip below.  This URL is upstream-controlled and may
# move or disappear without notice, which is precisely why it lives in a
# variable: override it to point at an HTTPS mirror rather than editing this
# script.
#
# HTTPS ONLY, and the check is fail-closed.  `is_safe_url` runs at the entry
# point of `main` and accepts nothing but `https://` followed by a non-empty
# remainder with no whitespace or control character in it, so `http://`,
# `file://` and every other scheme are refused before anything is read,
# downloaded or created.  If what you have is a local copy, either serve it
# over HTTPS or install it into the destination directory yourself and do not
# run this script -- there is deliberately no scheme here that reads a path.
ZLIB_RS_SILESIA_URL=${ZLIB_RS_SILESIA_URL:-https://sun.aei.polsl.pl/~sdeor/corpus/silesia.zip}

# The sentinel that means "no digest has been pinned yet".  It is deliberately
# not a valid SHA-256, so it can never be confused with one.
SILESIA_SHA256_SENTINEL='UNPINNED'

# ★ THE PIN FILE -- tier 3, and no longer a sentinel.
#
# This used to read `SILESIA_SHA256_EXPECTED="$SILESIA_SHA256_SENTINEL"`, i.e.
# the literal string UNPINNED, so a run that did not pass --sha256 or set the
# environment variable was refused.  That made the AAP's own acceptance corpus
# depend on something a person had to remember, and it let a required CI check
# go green having verified nothing.
#
# The digest now lives in ./silesia.pin, next to this script, committed and
# reviewed like code.  A file rather than a shell variable for two reasons: the
# workflow needs to read the same value to decide whether the acceptance tier is
# armed, and it can `grep` a `key=value` file without either it or this script
# having to parse the other; and a pin belongs with the size and member bounds it
# implies, which a single variable cannot hold.
#
# Overridable so a fork or a mirror can point at its own pin, but note that
# `--sha256` and the environment still take precedence -- see
# `select_expected_digest`, where all three tiers are ordered in one place.
ZLIB_RS_SILESIA_PIN=${ZLIB_RS_SILESIA_PIN:-}

# Populated by `load_pin` from the pin file.  Empty means no pin was readable,
# which is a refusal for a fetch and a note for `--pin-status`.
pin_path=''
pin_url=''
pin_sha256=''
pin_archive_bytes=''
pin_member_count=''
pin_expanded_bytes=''
pin_max_member_bytes=''
pin_compression_ratio=''

# Tier 3 of the digest sources, kept as a variable so the precedence chain in
# `select_expected_digest` reads the same way it always did.  Set from the pin
# file by `load_pin`; the sentinel survives only when no pin could be read.
SILESIA_SHA256_EXPECTED="$SILESIA_SHA256_SENTINEL"

# ★ Multiplier applied to the pinned bounds before they are enforced.
#
# The pin records the approved archive exactly.  Enforcing those numbers as hard
# equalities would refuse a mirror that recompressed the same twelve files, which
# is a legitimate thing to accept deliberately (that is what the per-member
# manifest in ./silesia.sha256 is for).  So the SIZE bounds are enforced with this
# much slack while the DIGEST stays exact: the slack is what makes the bounds a
# resource ceiling rather than a second, weaker identity check.
#
# Two, i.e. twice the approved size.  Large enough that no honest repackaging of
# the same corpus trips it, small enough that the ceiling still bounds the disk a
# hostile archive can consume to roughly 135 MB downloaded and 404 MiB expanded --
# which is the point of having one (CWE-400).
SILESIA_BOUND_SLACK=2

# Absolute fallback ceilings, used only when no pin is readable.
#
# `--verify-only` against a pre-provisioned directory does not need a pin, and a
# fork may legitimately delete one; neither case should leave the extraction
# unbounded.  These are the pinned figures times the slack above, written out, so
# that the no-pin path enforces exactly what the pinned path would have.
SILESIA_FALLBACK_ARCHIVE_BYTES=136365488
SILESIA_FALLBACK_EXPANDED_BYTES=423877160
SILESIA_FALLBACK_MEMBER_BYTES=102440960
SILESIA_FALLBACK_MEMBER_COUNT=24

# ★ Ceiling on declared-expanded-bytes divided by bytes actually downloaded.
#
# The bound that catches a zip bomb whose per-member and total declared sizes are each
# individually believable but whose expansion factor is not. The approved corpus
# expands 3.11x (see compression_ratio in the pin), so twenty is generous by a factor
# of six -- deliberately, because a mirror storing the corpus with a stronger
# compressor legitimately raises the ratio -- and it still refuses the classic bomb,
# which expands by six or more orders of magnitude.
SILESIA_MAX_EXPANSION_RATIO=20

# Per-invocation override of the expected digest, tier 2 of the three sources
# listed in the CHECKSUM POLICY block.  Case-insensitive; it must be exactly 64
# hexadecimal characters or the run is refused.  Left empty when unset so that
# `select_expected_digest` can tell the three tiers apart and name the one it
# used; the precedence lives there, in one place, rather than in this default.
ZLIB_RS_SILESIA_SHA256=${ZLIB_RS_SILESIA_SHA256:-}

# The marker this script writes inside the destination once a fetch has
# completed and verified.  Its presence -- not a guess about which member
# files the archive happens to contain -- is what makes a re-run idempotent,
# so upstream repackaging cannot confuse the check.  It is dot-prefixed to
# mark it as metadata rather than corpus data.
STAMP_NAME='.fetch_silesia.stamp'

# The pin file's name, beside this script.  See ZLIB_RS_SILESIA_PIN above and
# `load_pin` below for what it contains and how it is read.
PIN_NAME='silesia.pin'

# The per-member digest manifest's name, beside this script.  Offered as the
# default for --manifest so that the committed manifest is what a bare run checks
# against, rather than something a caller has to remember to point at.
MANIFEST_NAME='silesia.sha256'

# The ceilings `resolve_bounds` fills in, and the source it names in a diagnostic.
# Declared here, empty, so that every one of them is a defined name from the first
# line of `main` onward and a reference to one before it is resolved is an empty
# string rather than an unbound-variable failure under `set -u`.
max_archive_bytes=''
max_expanded_bytes=''
max_member_bytes=''
max_member_count=''
bounds_source=''

# The member files the Silesia corpus consists of, and the inventory the
# extracted payload is checked against.  Upstream publishes twelve files, each
# one a different data type, which is exactly what makes the corpus useful for
# throughput measurement:
#
#   dickens   English literature (Project Gutenberg)
#   mozilla   Mozilla 1.0 binary executables (Tru64 UNIX)
#   mr        medical magnetic resonance image
#   nci       structured chemical database
#   ooffice   OpenOffice.org 1.01 shared library (Windows DLL)
#   osdb      MySQL sample database
#   reymont   Polish literature, PDF
#   sao       star catalogue, binary
#   samba     Samba source distribution
#   webster   The 1913 Webster dictionary, HTML
#   x-ray     X-ray medical image
#   xml       collected XML documents
#
# Their individual provenances differ and are all third-party; this repository
# redistributes none of them.  See ./README.md, "Licensing", for what running
# this script commits you to.
#
# Override to accept a mirror that repackages the corpus under different names,
# or set it to `-` to skip the inventory check as a deliberate, visible choice.
ZLIB_RS_SILESIA_MEMBERS=${ZLIB_RS_SILESIA_MEMBERS:-dickens mozilla mr nci ooffice osdb reymont sao samba webster x-ray xml}

# Path to a `sha256sum`-format manifest of PER-MEMBER digests.  Every listed
# member is hashed and must match: after extraction on a fetch, and against the
# installed directory under --verify-only.
#
# WHAT THIS AUTHENTICATES THAT THE ARCHIVE DIGEST CANNOT.  Both checks can
# authenticate, and they authenticate different things, so neither is "the only
# one that does".  The archive digest below authenticates ONE EXACT ARCHIVE: from
# a value that reached you through a channel independent of the download, it
# proves you hold precisely those bytes.  What it cannot survive is repackaging --
# zip archives are not reproducible, so a rebuilt archive of the identical twelve
# files has a different digest and cannot be told apart from a hostile one.  A
# member manifest authenticates the EXTRACTED CONTENT, independently of how the
# archive was packed, so it carries across mirrors and rebuilds; obtained from a
# paper, a distribution package or a colleague's machine, it is independent
# evidence about the files themselves.  Both are trust-on-first-use if their
# value came from the download's own channel; neither bootstraps trust alone.
#
# ★ It used to say "unset by default, because this repository has no authenticated
# source for those digests to ship". It ships them now: ./silesia.sha256 carries all
# twelve, computed from the extracted contents of the archive whose digest
# ./silesia.pin records and verified with `sha256sum --check`. `main` selects that
# file when this variable is empty, so a bare run performs the per-member check
# rather than skipping it, and an explicit value still wins.
#
# The trust-on-first-use caveat above is unchanged and is worth restating plainly:
# these digests are evidence that the corpus has not changed since it was pinned
# here, not evidence that what was pinned is what the Silesia authors published. For
# the latter, obtain the digests independently and pass them.
#
# OPTIONAL ON A FETCH, REQUIRED BY --verify-only.  A fetch hashes the archive it
# downloaded against a pinned digest before unpacking it, so the members are
# established either way and a manifest is supplementary.  --verify-only fetches
# nothing, and the archive is not retained, so its digest can never be recomputed
# from an installed directory: the manifest is the only thing that mode can read
# which does not come from inside the directory it is being asked to vouch for.
# There, the manifest must cover every name in the inventory, and
# ZLIB_RS_SILESIA_MEMBER_SHA256=- is the only way to waive it -- after which the
# report says in so many words that it establishes nothing about the bytes.
ZLIB_RS_SILESIA_MEMBER_SHA256=${ZLIB_RS_SILESIA_MEMBER_SHA256:-}

# Basename used for the archive while it sits in the staging directory.  Kept
# free of shell- and sed-special characters on purpose: the digest is computed
# from inside the staging directory using this literal name, so no quirk of a
# user-supplied destination path can perturb the digest tool's output format.
ARCHIVE_NAME='silesia.zip'

# Assigned during the run; declared here so the whole mutable surface is visible in
# one place.  `preserve_stage` is the one with a non-obvious meaning: it is set only
# when an install failed AND the displaced previous contents could not be put back,
# which is the single case in which `cleanup` must keep the staging directory rather
# than delete it.
downloader=''
digest_tool=''
unpacker=''

# Set by the argument parser.  `opt_sha256` is tier 1 of the three expected-
# digest sources and outranks both the environment and the pinned constant.
opt_force=0
opt_verify_only=0
# Report what ./silesia.pin carries and exit, touching nothing.  See `pin_status`.
opt_pin_status=0
opt_sha256=''

# Set by `resolve_destination`: the directory the corpus is installed into, and
# whether that came from the default rather than from the environment.
destination=''
destination_is_default=0
stamp_url=''
stamp_sha256=''
stage_dir=''

# Where the destination's previous contents are parked while the replacement is
# installed, or empty when nothing has been displaced.
#
# This exists because the displaced data lives INSIDE the staging directory that
# `cleanup` deletes, so between the move-aside and the install there is a window
# in which a signal would take the caller's corpus with it.  `install_payload`
# sets this BEFORE the move rather than after -- if the move never happened the
# path simply does not exist, and every reader below tests for existence -- which
# leaves no instant where the data is displaced but unrecorded.  It is cleared
# only once the install is committed.
previous_dir=''

# Set when `cleanup` must keep the staging directory rather than delete it,
# because it holds displaced previous contents that could not be put back.
preserve_stage=0

# Location of this script and of the repository, derived without `readlink -f`
# (which is absent on macOS).  This script lives at
# crates/zlib-rs-differential/corpus/fetch_silesia.sh, so the repository root
# is exactly three directories above its own directory.
#
# `$0` is not canonicalised, so invoking a *symlink* to this script from
# outside the repository would derive the wrong root.  That is caught rather
# than silently tolerated: `resolve_destination` refuses a default destination
# whose derived root does not look like this repository, and tells you to set
# ZLIB_RS_SILESIA_DIR instead.
script_dir=$(cd "$(dirname "$0")" && pwd)
repo_root=$(cd "$script_dir/../../.." && pwd)

# Everything the script says about what it is doing goes through `info`, so a caller
# can silence all of it with one redirection.  Every failure path ends in `die`, so no
# failure can ever exit 0.
info() {
    printf '%s\n' "$*"
}

warn() {
    printf 'fetch_silesia.sh: warning: %s\n' "$*" >&2
}

die() {
    printf 'fetch_silesia.sh: error: %s\n' "$*" >&2
    exit 1
}

# `command -v` is the POSIX spelling of this test; `which` is not.
have() {
    command -v "$1" >/dev/null 2>&1
}

# So that a digest supplied in upper case compares equal.
lowercase() {
    printf '%s' "$1" | tr '[:upper:]' '[:lower:]'
}

# THE THREE GLOB CLASSES, AND WHY ALL THREE ARE NEEDED.
#
# Listing "every entry in a directory" with POSIX globs takes exactly three
# patterns, because `*` deliberately does not match a leading dot and the
# bracket expression that recovers dotfiles cannot cover both remaining shapes
# at once:
#
#   "$1"/*        every name not beginning with a dot
#   "$1"/.[!.]*   names beginning with one dot: .git, .keep, .DS_Store
#   "$1"/..?*     names beginning with two dots and continuing: ..keep, ...x
#
# The third is the one that is easy to leave out and it is not hypothetical:
# `..keep` and `..data` are real conventions (Kubernetes writes `..data` into
# every projected-volume mount), and `.[!.]*` cannot match them because its
# second character class explicitly excludes a dot.  Omitting it made
# `dir_is_empty` answer "empty" for a directory holding such a file, so
# `--force`-less runs would have written into a non-empty destination and
# `payload_present` would have missed existing data.
#
# `.` and `..` themselves are matched by none of the three: `*` excludes them
# by the leading dot, `.[!.]*` needs a non-dot second character, and `..?*`
# needs at least one character after the second dot.  That is the whole reason
# for this particular spelling rather than a simpler `.*`.
#
# An unmatched POSIX glob expands to itself, so each candidate is tested for
# existence rather than counted; `-L` is tested as well so that a dangling
# symlink still counts as an entry, because it is one.
# True when directory $1 contains no entries at all, dotfiles included.
dir_is_empty() {
    for _entry in "$1"/* "$1"/.[!.]* "$1"/..?*; do
        if [ -e "$_entry" ] || [ -L "$_entry" ]; then
            return 1
        fi
    done
    return 0
}

# True when directory $1 contains at least one entry that is not the stamp,
# i.e. when it holds actual corpus data.
payload_present() {
    for _entry in "$1"/* "$1"/.[!.]* "$1"/..?*; do
        if [ ! -e "$_entry" ] && [ ! -L "$_entry" ]; then
            continue
        fi
        case "${_entry##*/}" in
            "$STAMP_NAME") continue ;;
            *) return 0 ;;
        esac
    done
    return 1
}

# Usage.  Written to stdout for `--help` so it can be piped; the argument-error
# path redirects it to stderr instead.
usage() {
    cat <<'EOF'
Usage: fetch_silesia.sh [--sha256 <digest>] [--force] [--verify-only]
       fetch_silesia.sh --pin-status
       fetch_silesia.sh [-h|--help]

Fetches the Silesia compression corpus for the repository-root benchmark
suites.  OPT-IN ONLY: FETCHING is something a human does deliberately, nothing
in this repository does it for you, and no correctness gate depends on what it
produces.  Running it is never required -- `cargo test` does not need it, and
`cargo bench` reports and skips when the corpus is absent.

VERIFYING is what CI does do with this script, in two modes and neither of them a
fetch: the `bench` job of .github/workflows/rust.yml runs --pin-status on every
runner, and --verify-only against a corpus that was provisioned outside the
workflow, from a cache keyed on the pinned digest or from ZLIB_RS_SILESIA_DIR.
Both reach no network, write nothing and obtain nothing; the workflow fails closed
rather than downloading.

An expected SHA-256 is REQUIRED on every fetch or verify operation, --verify-only
included.  (--help and an argument error are not operations on a corpus and need
none.)  The committed ./silesia.pin supplies one, so no flag and no variable is
needed for the archive that pin approves; --sha256 and ZLIB_RS_SILESIA_SHA256
outrank it, for a mirror or a repackaging with a different digest.  If the pin is
absent and neither is given, the run is refused rather than trusted -- upstream
publishes no digest for this archive and zip archives are not reproducible, so
there is no value that could be assumed.  See the CHECKSUM POLICY block at the top
of this script for the full reasoning, and --pin-status for what is pinned now.

Options:
  --sha256 <hex> Expected SHA-256 of the archive: exactly 64 hex digits,
                 case-insensitive.  Outranks ZLIB_RS_SILESIA_SHA256, which in
                 turn outranks ./silesia.pin.  Optional only because the pin
                 supplies a value; there is no way to skip the check and no
                 value that disables it.
  --force        Re-fetch even when the destination is already populated,
                 replacing whatever is there.  Also required to overwrite a
                 populated destination that carries no stamp from a previous
                 successful run.  It does not weaken verification: the fresh
                 download is checked exactly as any other.
  --pin-status   Print what ./silesia.pin records -- the approved archive's
                 URL, digest, size and member bounds -- and the ceilings this
                 run would enforce, then exit.  Downloads nothing, creates
                 nothing and reads no destination, so it is safe to run
                 anywhere.  This is what CI reads to decide whether the
                 acceptance tier is armed.
  --verify-only  Report on an existing download and exit without fetching
                 anything.  Exits non-zero when the destination is missing,
                 incomplete, carries an unreadable stamp, or was fetched from
                 a URL or digest other than the ones expected now.
                 "Incomplete" is judged against ZLIB_RS_SILESIA_MEMBERS, the
                 same inventory a fetch applies, so a destination holding
                 eleven of the twelve members fails here.
                 REQUIRES ZLIB_RS_SILESIA_MEMBER_SHA256, because everything
                 else this mode can read -- the stamp, the names, the
                 inventory -- comes from inside the directory being vouched
                 for, and the archive is not retained after a fetch, so its
                 digest cannot be recomputed here.  With a manifest every
                 member is hashed and compared, and the manifest must cover
                 the whole inventory.  Set the variable to `-` to waive that
                 and accept a stamp-and-inventory report instead; the output
                 then says so explicitly.
  -h, --help     Print this text and exit 0.  Touches no network and creates
                 no files.

Requirements: curl or wget; sha256sum or shasum; unzip or bsdtar.  GNU tar
cannot read zip archives and is not a substitute.  A minimal container image
often has none of the three.

Environment:
  ZLIB_RS_SILESIA_DIR      Destination directory.  Unset or empty selects
                           <repo-root>/target/silesia, which the root
                           .gitignore already ignores.  The benchmarks read
                           this same variable, so set it for them too.
  ZLIB_RS_SILESIA_URL      Source archive URL.  Must begin with https:// and
                           carry no whitespace or control character; every
                           other scheme, http:// and file:// included, is
                           refused before anything is downloaded.  Override it
                           to use an HTTPS mirror, and change it and the
                           expected digest together: they identify one
                           archive, jointly.
  ZLIB_RS_SILESIA_SHA256   Expected SHA-256, as for --sha256.  Used when
                           --sha256 is absent; itself overridden by nothing.
                           REQUIRED unless --sha256 is given: no digest is
                           pinned in this script, so a fetch that supplies
                           neither is refused.  See the CHECKSUM POLICY
                           block for how to obtain a value and for what a
                           digest does and does not establish.
  ZLIB_RS_SILESIA_MEMBERS  Space-separated list of the file names the extracted
                           payload must contain, exactly.  Defaults to the
                           twelve Silesia members.  Set to `-` to skip the
                           inventory check.
  ZLIB_RS_SILESIA_MEMBER_SHA256
                           Path to a sha256sum-format manifest of per-member
                           digests.  Every listed member is hashed and must
                           match.  Optional on a fetch, which verifies the
                           archive it downloaded; REQUIRED by --verify-only,
                           which downloads nothing and where the manifest must
                           cover every member named by ZLIB_RS_SILESIA_MEMBERS.
                           Set it to `-` to waive the check deliberately and
                           visibly.  Defaults to the committed ./silesia.sha256.

                           What it adds, stated exactly, because the two checks
                           authenticate DIFFERENT things and neither is the only
                           one that authenticates.  The archive digest
                           (--sha256) authenticates one exact archive: if the
                           value came from a channel you trust, matching it
                           proves you have the very bytes that value describes,
                           and no substitution or corruption in transit can
                           pass.  What it cannot survive is repackaging -- zip
                           archives are not reproducible, so a rebuilt archive
                           of the identical twelve files has a different digest
                           and is indistinguishable from a hostile one.  This
                           manifest authenticates the EXTRACTED CONTENT instead:
                           it is independent of how the archive was packed, so
                           it carries across mirrors and rebuilds where the
                           archive digest cannot, and it is the only expectation
                           --verify-only can read that does not come from inside
                           the directory being checked.  Both depend on the
                           value reaching you through a trustworthy channel;
                           neither can bootstrap trust on its own.

Space: roughly 65 MiB downloaded and a few hundred megabytes unpacked; allow
about 300 MiB free in the destination's filesystem.

Examples.  Every fetching invocation carries a digest, because a fetch with
neither --sha256 nor ZLIB_RS_SILESIA_SHA256 is refused.  Substitute the 64-hex
value you obtained and cross-checked:
  # Fetch into <repo-root>/target/silesia.  Substitute the digest you trust.
  ./crates/zlib-rs-differential/corpus/fetch_silesia.sh \
      --sha256 0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef

  # A shared cache outside the repository, digest from the environment.
  ZLIB_RS_SILESIA_DIR=/var/cache/silesia \
  ZLIB_RS_SILESIA_SHA256=0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef \
      ./crates/zlib-rs-differential/corpus/fetch_silesia.sh

  # Confirm an existing download is the one you expect, without refetching.
  # The manifest is what makes this a statement about the bytes; replace it
  # with ZLIB_RS_SILESIA_MEMBER_SHA256=- to accept a stamp-only report.
  ZLIB_RS_SILESIA_DIR=/var/cache/silesia \
  ZLIB_RS_SILESIA_MEMBER_SHA256=/path/to/silesia.sha256 \
      ./crates/zlib-rs-differential/corpus/fetch_silesia.sh --verify-only \
      --sha256 0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef

  # Then measure.  The benchmarks read the same destination variable.

  # Additionally authenticate each extracted member against a manifest.
  ZLIB_RS_SILESIA_MEMBER_SHA256=/path/to/silesia.sha256 \
      ./crates/zlib-rs-differential/corpus/fetch_silesia.sh \
      --sha256 0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef
  ZLIB_RS_SILESIA_DIR=/var/cache/silesia cargo bench --manifest-path benches/Cargo.toml

See ./README.md for the corpus contract this script implements.
EOF
}

# Argument parsing.  Unknown arguments are refused rather than ignored: a typo
# in a flag must not silently turn into a default-configuration fetch.
parse_arguments() {
    while [ "$#" -ge 1 ]; do
        case "$1" in
            -h | --help)
                usage
                exit 0
                ;;
            --force)
                opt_force=1
                shift
                ;;
            --pin-status)
                opt_pin_status=1
                shift
                ;;
            --verify-only)
                opt_verify_only=1
                shift
                ;;
            # Both spellings, because `--sha256=<hex>` is what people type.
            # The value is validated centrally in `select_expected_digest`, so
            # a malformed one is reported the same way from every source.
            --sha256)
                [ "$#" -ge 2 ] || die "--sha256 requires a 64-hex-digit value"
                opt_sha256=$2
                shift 2
                ;;
            --sha256=*)
                opt_sha256=${1#--sha256=}
                shift
                ;;
            *)
                usage >&2
                die "unrecognized argument: $1"
                ;;
        esac
    done

    if [ "$opt_force" -eq 1 ] && [ "$opt_verify_only" -eq 1 ]; then
        die "--force and --verify-only contradict each other; pick one"
    fi
}

# True when $1 -- `curl' or `wget' -- accepts the option this script uses to
# confine HTTP redirects to https://.
#
# Asked of the installed binary rather than derived from a version number:
# distributions patch both tools, and `--help' is the only authority on what the
# program in $PATH actually accepts.  curl's option list moved behind
# `--help all' in 7.74, so both spellings are consulted and the output of the
# two is searched together; `--help' short-circuits before any transfer in every
# curl that has ever shipped, so neither invocation reaches the network.
downloader_confines_redirects() {
    case "$1" in
        curl)
            { curl --help all 2>/dev/null; curl --help 2>/dev/null; } |
                grep -q -- '--proto-redir'
            ;;
        wget)
            # BOTH options, because under wget the guarantee takes both. --https-only
            # constrains the scheme but was MEASURED not to stop a 302 downgrade -- it
            # governs recursive link following -- so the redirect is refused outright
            # with --max-redirect=0, and a wget that does not accept that option cannot
            # make the guarantee either.
            wget --help 2>/dev/null | grep -q -- '--https-only' &&
                wget --help 2>/dev/null | grep -q -- '--max-redirect'
            ;;
        *) return 1 ;;
    esac
}

# SHA-256 tool probing, on its own so that --verify-only can reach it without
# also demanding a downloader and a zip extractor it will never use.  Idempotent
# and cheap, so callers may probe defensively.
#
# sha256sum is coreutils; shasum ships with macOS, which has no sha256sum.
probe_digest_tool() {
    if [ -n "$digest_tool" ]; then
        return 0
    fi

    if have sha256sum; then
        digest_tool='sha256sum'
    elif have shasum; then
        digest_tool='shasum'
    else
        die "no SHA-256 tool found; install sha256sum (coreutils) or shasum"
    fi
}

# Tool probing.  Each family degrades to a sensible alternative and otherwise
# fails closed with a message naming what to install.  All of it happens before
# any network access or filesystem mutation, so a machine missing a tool learns
# that immediately instead of after a 65 MiB download.
probe_tools() {
    # HTTPS is required of the URL *and* of every hop a redirect leads through,
    # so the tool selected here has to be able to enforce the second half.  Both
    # tools follow redirects by default, and an allowlisted scheme on the command
    # line buys nothing on its own: a 302 to http:// would be followed silently,
    # which is precisely the downgrade `validate_url' refuses to accept in the
    # value it was handed.  curl expresses the restriction as --proto/
    # --proto-redir; wget cannot express "follow only https redirects" at all --
    # --https-only was measured following a 302 downgrade, because it governs
    # recursive link following -- so under wget the hop is refused outright with
    # --max-redirect=0.  A build of either tool that does not accept what it needs
    # cannot make the guarantee and is passed over rather than used without it.
    if have curl && downloader_confines_redirects curl; then
        downloader='curl'
    elif have wget && downloader_confines_redirects wget; then
        downloader='wget'
    elif have curl || have wget; then
        die "a downloader is installed, but not one that can confine redirects to
       https://: curl needs --proto-redir (7.20.0 and later) and wget needs both
       --https-only and --max-redirect, and neither tool accepts what it needs here.
       Without it a redirect could move this download to plain http, so the
       fetch is refused rather than performed unprotected.  Install a current
       curl, or provision the corpus yourself and point ZLIB_RS_SILESIA_DIR at
       it -- --verify-only needs no downloader at all."
    else
        die "no downloader found; install curl (preferred) or wget"
    fi

    probe_digest_tool

    # The archive is a zip.  GNU tar cannot read zip archives, so it is not an
    # acceptable fallback here and is deliberately not probed for; bsdtar
    # (libarchive) can, and is the usual second choice.
    if have unzip; then
        unpacker='unzip'
    elif have bsdtar; then
        unpacker='bsdtar'
    else
        die "no zip extractor found; install unzip (preferred) or bsdtar
       (libarchive-tools on Debian and Ubuntu, libarchive elsewhere).
       GNU tar cannot read zip archives and is not a substitute."
    fi
}

# Digest handling.
#
# These set globals rather than printing their result, because a `die` inside a
# command substitution would only terminate the subshell.  Failing loudly and
# exactly once, from the shell that can actually exit, is worth the global.

# Normalised expected digest, set by `resolve_expected_digest`.
expected_sha256=''

# Which of the three sources supplied it, for use in diagnostics.  Knowing
# whether a mismatch came from the command line, the environment or the pinned
# constant is most of the work of understanding the mismatch.
expected_sha256_source=''

# Digest of the file last passed to `compute_digest`.
computed_sha256=''

# True when $1 is exactly 64 hexadecimal characters.
is_sha256() {
    case "$1" in
        *[!0-9a-f]*) return 1 ;;
        *) ;;
    esac
    [ "${#1}" -eq 64 ]
}

# True when $1 is an acceptable https:// URL for the archive download.
#
# The download URL is caller-supplied through ZLIB_RS_SILESIA_URL, so it is
# untrusted input that ends up in a command line.  Two distinct problems are
# closed here, and the `--` added at both call sites closes only the first:
#
#   1. A value beginning with `-` is read by curl and wget as OPTIONS rather
#      than as a URL.  `--o /etc/cron.d/x` or `--config <file>` turns a corpus
#      fetch into an arbitrary write or an arbitrary-options invocation.  `--`
#      stops option parsing; requiring the `https://` prefix here means the
#      value cannot begin with `-` in the first place.
#   2. A whitespace or control character in the value.  These would not survive
#      the quoting as separate arguments, but they can still confuse a log,
#      split a header, or smuggle a newline into the stamp file this script
#      writes -- the stamp is a key=value format and a newline in the URL would
#      forge additional fields.
#
# `https://` is required rather than merely allowed.  The archive is verified by
# pinned SHA-256 afterwards, so plain http would still be *detected* if it were
# tampered with, but detection afterwards is not a reason to accept cleartext,
# and refusing `file://` and every other scheme keeps this from becoming a way to
# make the script read an arbitrary local path.
#
# WHAT THIS CHECK CANNOT DO, so that it is not mistaken for the whole of the
# transport policy: it sees only the value the caller supplied, and both
# downloaders follow redirects.  An https:// value that is answered with
# `Location: http://...` would satisfy every test in this function.  The hop is
# constrained where the hop is visible -- `download_archive` passes
# `--proto '=https' --proto-redir '=https'` to curl and `--https-only` to wget --
# and the two halves are only a policy together.
is_safe_url() {
    # Scheme, and a non-empty remainder after it.
    case "$1" in
        https://?*) ;;
        *) return 1 ;;
    esac

    # Reject space, tab and every C0/DEL control character.  The bracket
    # expression is spelled with explicit classes so it does not depend on the
    # locale's collation order.
    case "$1" in
        *[[:space:][:cntrl:]]*) return 1 ;;
        *) ;;
    esac

    return 0
}

# True when archive member path $1 is safe to extract.
#
# Applied to every member BEFORE anything is written, so that an escape is
# refused rather than merely detected afterwards.  Rejected:
#
#   empty            a member with no name at all
#   /...             absolute, which unzip strips but bsdtar can honour
#   ..  ../x  x/..   any `..` component, the classic escape
#   x\y              a backslash.  Info-ZIP translates backslashes to slashes
#                    on some platforms, so `..\..\etc` is a traversal that a
#                    slash-only check would wave through
#   control chars    unprintable names, which no legitimate corpus member has
#                    and which exist to make an audit trail unreadable
#
# Member TYPE -- symlink, hard link, device, socket -- is not checked here,
# because neither extractor lists types in a format that is stable across
# versions.  That check is done after extraction by `assert_tree_is_plain`,
# which inspects the filesystem instead and is therefore extractor-agnostic.
is_safe_member() {
    [ -n "$1" ] || return 1

    case "$1" in
        /*) return 1 ;;
        # A leading backslash, an embedded one and a Windows drive prefix are all
        # traversals on a Windows host: unzip 6.00 creates `..\..\escape' under
        # that literal name, which resolves as a climb there.
        \\*) return 1 ;;
        *\\*) return 1 ;;
        [A-Za-z]:*) return 1 ;;
        ..) return 1 ;;
        ../*) return 1 ;;
        */..) return 1 ;;
        */../*) return 1 ;;
        *[[:cntrl:]]*) return 1 ;;
        *) ;;
    esac

    return 0
}

# Pick the expected digest from the three sources documented in the CHECKSUM
# POLICY block, in order: --sha256, then the environment, then the pinned
# constant.  Sets `expected_sha256` to the raw value and
# `expected_sha256_source` to a human description of where it came from.
# Returns 1, without diagnosing, when no source supplied one.
#
# The sentinel is treated as "absent" wherever it appears, so a half-finished
# pin cannot masquerade as a digest.
# ★ Read ./silesia.pin into the `pin_*` variables, and tier 3 of the digest chain.
#
# Deliberately hand-parsed with `case` and `read` rather than sourced. Sourcing a
# configuration file executes it, so a pin file would become a place to run code
# from -- which is the wrong property for the one file whose whole job is to be
# trusted. This reads `key=value`, ignores `#` comments and blank lines, refuses a
# key it does not know, and assigns nothing it did not recognise.
#
# Every numeric field is validated here rather than where it is used, so a
# malformed pin fails once, early, and by name.
#
# Returns 0 when a pin was read, 1 when no pin file exists. A pin file that exists
# but is malformed is fatal: silently continuing without bounds would be worse than
# having none, because the caller would believe it had them.
load_pin() {
    if [ -n "$ZLIB_RS_SILESIA_PIN" ]; then
        pin_path=$ZLIB_RS_SILESIA_PIN
    else
        pin_path="${script_dir}/${PIN_NAME}"
    fi

    if [ ! -f "$pin_path" ]; then
        # A fork may legitimately delete it, and `--verify-only` against a
        # pre-provisioned directory does not need one. The fallback ceilings still
        # apply, so the extraction is bounded either way.
        if [ -n "$ZLIB_RS_SILESIA_PIN" ]; then
            die "the pin file named by ZLIB_RS_SILESIA_PIN does not exist:
       '$pin_path'

       Naming a pin that is not there is a mistake rather than a request to
       proceed without one, so this is refused.  Unset the variable to fall
       back to ${PIN_NAME} beside this script."
        fi
        pin_path=''
        return 1
    fi

    # `IFS=` and `-r` so a value keeps its spaces and its backslashes; the
    # `|| [ -n "$_line" ]` tail reads a final line with no newline after it.
    while IFS= read -r _line || [ -n "$_line" ]; do
        case "$_line" in
            '#'* | '') continue ;;
        esac
        _key=${_line%%=*}
        _value=${_line#*=}
        case "$_key" in
            url) pin_url=$_value ;;
            sha256) pin_sha256=$(lowercase "$_value") ;;
            archive_bytes) pin_archive_bytes=$_value ;;
            member_count) pin_member_count=$_value ;;
            expanded_bytes) pin_expanded_bytes=$_value ;;
            max_member_bytes) pin_max_member_bytes=$_value ;;
            compression_ratio) pin_compression_ratio=$_value ;;
            *)
                die "$pin_path names a field this script does not know:
       '$_key'

       A pin the script cannot fully interpret is refused rather than
       partly applied: the fields it did not understand might have been
       the bounds.  Add the field to load_pin, or remove it."
                ;;
        esac
    done < "$pin_path"

    if ! is_sha256 "$pin_sha256"; then
        die "$pin_path does not carry a valid 64-character hexadecimal
       sha256= field: '$pin_sha256'"
    fi
    for _field in archive_bytes:"$pin_archive_bytes" \
        member_count:"$pin_member_count" \
        expanded_bytes:"$pin_expanded_bytes" \
        max_member_bytes:"$pin_max_member_bytes"; do
        _name=${_field%%:*}
        _number=${_field#*:}
        if ! is_positive_integer "$_number"; then
            die "$pin_path carries a $_name= field that is not a positive
       integer: '$_number'"
        fi
    done
    case "$pin_url" in
        https://?*) ;;
        *)
            die "$pin_path carries a url= field that is not an https URL:
       '$pin_url'

       A digest without the URL it was taken from asserts nothing about
       where the bytes came from, so the two are validated together."
            ;;
    esac

    SILESIA_SHA256_EXPECTED=$pin_sha256
    return 0
}

# Whether $1 is a non-empty run of digits denoting a positive value.
#
# Written out rather than delegated to `test -gt`, which accepts leading `+`, a
# leading `-`, and surrounding whitespace on some shells, and whose failure mode on
# a non-number is a diagnostic on stderr rather than a false return.
is_positive_integer() {
    case "$1" in
        '' | *[!0-9]*) return 1 ;;
    esac
    [ "$1" != "0" ] && return 0
    return 1
}

# Multiply $1 by SILESIA_BOUND_SLACK, the ceiling actually enforced for a bound the
# pin states exactly.  See SILESIA_BOUND_SLACK for why the size bounds get slack
# while the digest does not.
bound_with_slack() {
    printf '%s' "$(( $1 * SILESIA_BOUND_SLACK ))"
}

# ★ The four ceilings this run will enforce, resolved once from the pin or from the
# fallbacks, into `max_*`.
#
# Resolved in one place so that no call site can enforce a bound the others do not
# know about, and so the diagnostic each violation prints can name where its number
# came from.
resolve_bounds() {
    if [ -n "$pin_archive_bytes" ]; then
        max_archive_bytes=$(bound_with_slack "$pin_archive_bytes")
        max_expanded_bytes=$(bound_with_slack "$pin_expanded_bytes")
        max_member_bytes=$(bound_with_slack "$pin_max_member_bytes")
        max_member_count=$(bound_with_slack "$pin_member_count")
        bounds_source="${PIN_NAME} x ${SILESIA_BOUND_SLACK}"
    else
        max_archive_bytes=$SILESIA_FALLBACK_ARCHIVE_BYTES
        max_expanded_bytes=$SILESIA_FALLBACK_EXPANDED_BYTES
        max_member_bytes=$SILESIA_FALLBACK_MEMBER_BYTES
        max_member_count=$SILESIA_FALLBACK_MEMBER_COUNT
        bounds_source="this script's built-in fallbacks"
    fi
}

select_expected_digest() {
    expected_sha256=''
    expected_sha256_source=''

    if [ -n "$opt_sha256" ] && [ "$opt_sha256" != "$SILESIA_SHA256_SENTINEL" ]; then
        expected_sha256=$opt_sha256
        expected_sha256_source='--sha256'
    elif [ -n "$ZLIB_RS_SILESIA_SHA256" ] &&
        [ "$ZLIB_RS_SILESIA_SHA256" != "$SILESIA_SHA256_SENTINEL" ]; then
        expected_sha256=$ZLIB_RS_SILESIA_SHA256
        expected_sha256_source='ZLIB_RS_SILESIA_SHA256'
    elif [ -n "$SILESIA_SHA256_EXPECTED" ] &&
        [ "$SILESIA_SHA256_EXPECTED" != "$SILESIA_SHA256_SENTINEL" ]; then
        expected_sha256=$SILESIA_SHA256_EXPECTED
        expected_sha256_source='SILESIA_SHA256_EXPECTED in this script'
    else
        return 1
    fi

    return 0
}

# Normalise and validate the expected digest into `expected_sha256`, failing
# closed when no source supplied one.  There is no path through this function
# that leaves verification disabled, and every caller that is about to trust
# the corpus -- fetching it, or vouching for an existing copy -- runs it first.
resolve_expected_digest() {
    if ! select_expected_digest; then
        die "no expected SHA-256 was supplied, so this run is refused.  An
       unverified corpus is not acceptable and there is deliberately no
       flag to skip verification.

       Supply it, in whichever way suits you:
           $0 --sha256 <64-hex-digest>
           ZLIB_RS_SILESIA_SHA256=<64-hex-digest> $0
       or pin it once in SILESIA_SHA256_EXPECTED near the top of this
       script, alongside the URL it belongs to.

       To obtain it, on a machine and over a channel you trust:
           curl -fSL -o silesia.zip '$ZLIB_RS_SILESIA_URL'
           sha256sum silesia.zip        # or: shasum -a 256 silesia.zip
       then cross-check that value against a second, independent
       retrieval before trusting it.  The CHECKSUM POLICY block in this
       script explains why no digest ships pinned."
    fi

    _raw=$expected_sha256
    expected_sha256=$(lowercase "$_raw")

    if ! is_sha256 "$expected_sha256"; then
        die "the expected SHA-256 from $expected_sha256_source is not a
       64-character hexadecimal digest: '$_raw'"
    fi
}

# Run the selected digest tool on $1, printing its raw output line.
run_digest_tool() {
    case "$digest_tool" in
        sha256sum) sha256sum "$1" ;;
        shasum) shasum -a 256 "$1" ;;
        *) return 1 ;;
    esac
}

# Compute the SHA-256 of file $2 inside directory $1 into `computed_sha256`.
#
# The tool is run from within that directory against the bare basename on
# purpose.  Both sha256sum and shasum escape their output line when the path
# contains a backslash or a newline, which would corrupt the field extraction
# below; a user-supplied destination path can contain either, whereas a
# basename this script chose itself cannot.
compute_digest() {
    if ! _output=$(cd "$1" && run_digest_tool "$2"); then
        die "$digest_tool could not read $1/$2"
    fi

    # The output is "<digest>  <name>", so the first field is the digest.
    computed_sha256=${_output%% *}

    if ! is_sha256 "$computed_sha256"; then
        die "$digest_tool produced output this script cannot parse: '$_output'"
    fi
}

# Destination resolution -- the one canonical rule, reproduced from
# ./README.md, section "The Silesia tier: path contract".  The benchmarks
# resolve the directory the same way; the two must never drift apart.
#
#   1. $ZLIB_RS_SILESIA_DIR, when set and non-empty.
#   2. Otherwise <repo-root>/target/silesia.
#
# The default sits under target/ because the root .gitignore ignores /target/,
# so a fetched corpus can never be committed by accident.  A user-supplied
# directory is taken at face value and deliberately not checked against the
# ignore rules: it is normal for it to live outside the repository altogether,
# as a shared cache does, and second-guessing an explicit choice would be
# worse than honouring it.  The default is the safe one.
resolve_destination() {
    if [ -n "$ZLIB_RS_SILESIA_DIR" ]; then
        destination="$ZLIB_RS_SILESIA_DIR"
        destination_is_default=0
        normalise_destination
        return 0
    fi

    # Only the default depends on the derived repository root, so only the
    # default needs the root to be plausible.  zlib.h is the immutable public
    # contract of this project and is guaranteed to sit at its top level.
    if [ ! -f "$repo_root/zlib.h" ]; then
        die "cannot locate the repository root from '$0'
       (derived '$repo_root', which does not contain zlib.h).
       This happens when the script is reached through a symlink, since \$0
       is not canonicalised.  Run the real file, or name the destination
       explicitly:
           ZLIB_RS_SILESIA_DIR=<dir> $0"
    fi

    destination="$repo_root/target/silesia"
    destination_is_default=1
}

# Put the resolved destination into a shape the rest of the script can rely on:
# absolute, so the path reported on success is unambiguous from any working
# directory, and free of a trailing slash, because `mv src dir/` demands that
# `dir` already exist and would fail on the very first install.  Nothing is
# created here -- resolution stays free of side effects so that `--help` and
# `--verify-only` remain side-effect free.
normalise_destination() {
    while :; do
        case "$destination" in
            */) destination=${destination%/} ;;
            *) break ;;
        esac
    done

    if [ -z "$destination" ]; then
        die "ZLIB_RS_SILESIA_DIR resolves to the filesystem root; refusing to
       install a corpus there"
    fi

    case "${destination##*/}" in
        . | ..)
            die "ZLIB_RS_SILESIA_DIR must name the corpus directory itself, not
       '.' or '..': $ZLIB_RS_SILESIA_DIR"
            ;;
        *) ;;
    esac

    # Relative paths are taken relative to the current directory, which is what
    # a person typing one expects.  The result is absolute but not canonical --
    # any '..' the caller wrote is left in place, since resolving it would mean
    # either readlink -f, which macOS lacks, or touching the filesystem.
    case "$destination" in
        /*) ;;
        *) destination="$(pwd)/$destination" ;;
    esac
}

# Print the value of key $2 from stamp file $1, or nothing when it is absent.
# The keys are internal literals, so no quoting hazard arises from the
# interpolation into sed's script.
stamp_field() {
    sed -n "s/^$2=//p" "$1" | sed -n '1p'
}

# Write the stamp into directory $1.  Recording the URL and the verified digest
# is what lets a later run -- or a puzzled human -- tell exactly which archive
# produced the contents of the directory.
write_stamp() {
    _fetched=$(date -u '+%Y-%m-%dT%H:%M:%SZ' 2>/dev/null || printf 'unknown')

    # The redirection below FOLLOWS a symlink, so if anything has managed to put
    # one at this path the write lands wherever it points -- with the invoking
    # user's permissions, on a path they never named.  An extracted archive is
    # the obvious way for that to happen, and `assert_tree_is_plain` already
    # refuses any archive containing a link; this is the second lock on the same
    # door, positioned at the actual write.  Any non-regular entry is refused for
    # the same reason: a fifo would block, a device would be written through.
    if [ -L "$1/$STAMP_NAME" ]; then
        die "refusing to write the stamp: $1/$STAMP_NAME is a symbolic link"
    fi
    if [ -e "$1/$STAMP_NAME" ] && [ ! -f "$1/$STAMP_NAME" ]; then
        die "refusing to write the stamp: $1/$STAMP_NAME is not a regular file"
    fi

    {
        printf '%s\n' '# Written automatically by fetch_silesia.sh. Do not edit.'
        printf '%s\n' '# Its presence marks this directory as a complete, verified fetch;'
        printf '%s\n' '# delete it, or pass --force, to make the script fetch again.'
        printf '%s\n' 'format=1'
        printf 'url=%s\n' "$ZLIB_RS_SILESIA_URL"
        printf 'sha256=%s\n' "$expected_sha256"
        printf 'fetched=%s\n' "$_fetched"
        printf 'tools=%s\n' "$downloader,$digest_tool,$unpacker"
    } > "$1/$STAMP_NAME" || die "could not write the stamp into $1"
}

# Inspect the stamp in directory $1.  Sets `stamp_url` and `stamp_sha256` and
# returns 0 when the stamp exists and is well formed; returns 1 otherwise,
# without diagnosing, so callers can decide whether that is fatal.
read_stamp() {
    stamp_url=''
    stamp_sha256=''

    [ -f "$1/$STAMP_NAME" ] || return 1
    [ -r "$1/$STAMP_NAME" ] || return 1

    # Only format 1 has ever been written.  Refusing anything else means a
    # future format cannot be silently misread by an older copy of this script.
    _format=$(stamp_field "$1/$STAMP_NAME" format)
    [ "$_format" = '1' ] || return 1

    stamp_url=$(stamp_field "$1/$STAMP_NAME" url)
    stamp_sha256=$(stamp_field "$1/$STAMP_NAME" sha256)

    [ -n "$stamp_url" ] || return 1
    is_sha256 "$stamp_sha256" || return 1

    return 0
}

# Staging and cleanup.
#
# `mktemp -d` is anchored beside the destination rather than left in $TMPDIR so
# that the final install is a rename within one filesystem: atomic, and without
# copying a few hundred megabytes across a device boundary.  The cleanup runs
# on normal exit and on interruption alike, so an abandoned run leaves neither
# a partial destination nor a stray archive.
#
# The signal traps exit explicitly.  A POSIX shell resumes at the next command
# after a trap handler returns, so a bare cleanup handler would let an
# interrupted run carry on into the verification step with a truncated
# download; the conventional 128+signo statuses make the interruption visible
# to whatever invoked the script.
cleanup() {
    if [ -z "$stage_dir" ] || [ ! -d "$stage_dir" ]; then
        return 0
    fi

    # FIRST, rescue anything this script displaced.
    #
    # `install_payload` moves an existing destination into the staging directory
    # before putting the replacement in its place, which means that between those
    # two renames the caller's corpus lives inside the directory this function
    # deletes.  A SIGINT in that window used to run straight past this point to
    # the `rm -rf` below and take the displaced data with it: the previous
    # contents were only ever marked for preservation on the *failure* path of
    # the restore, which an interrupted run never reaches.
    #
    # So the displaced data is looked for here, on every exit and every signal.
    # Restoring is preferred, and is safe precisely because the destination not
    # existing is the condition being tested -- so putting it back cannot
    # clobber a newly installed corpus, and cannot race the install's own rename
    # (either that rename has happened, in which case the destination exists and
    # this is skipped, or it has not, in which case the path is free).  If the
    # restore is impossible the data is kept and reported rather than deleted:
    # leaving a stray directory behind is a nuisance, losing the corpus is not.
    if [ -n "$previous_dir" ] && [ -e "$previous_dir" ]; then
        if [ ! -e "$destination" ] && mv "$previous_dir" "$destination"; then
            previous_dir=''
            warn "interrupted; the previous contents of $destination were
         restored"
        else
            preserve_stage=1
        fi
    fi

    # One case must not be cleaned up: displaced previous contents that could
    # not be put back. Deleting the staging directory then would destroy data
    # this script displaced, so it is kept and its location reported instead.
    # Everything else goes.
    if [ "$preserve_stage" -eq 1 ]; then
        printf '%s\n' "fetch_silesia.sh: the previous contents of the destination
       were left in $stage_dir because they could not be restored.
       Move them back or delete that directory by hand." >&2
        return 0
    fi

    rm -rf "$stage_dir"
}

install_traps() {
    trap 'cleanup' EXIT
    trap 'cleanup; exit 130' INT
    trap 'cleanup; exit 143' TERM
    trap 'cleanup; exit 129' HUP
}

stage_create() {
    _parent=$(dirname "$destination")

    # The destination's parent, and nothing else, is created up front: the
    # destination itself only ever comes into existence as a completed rename.
    mkdir -p "$_parent" || die "could not create $_parent"

    stage_dir=$(mktemp -d "$_parent/.fetch_silesia.XXXXXX") ||
        die "could not create a staging directory in $_parent"
}

# -----------------------------------------------------------------------------
# Fetch, verify, unpack, install.
# -----------------------------------------------------------------------------

# THE URL IS UNTRUSTED INPUT, AND QUOTING IT IS NOT ENOUGH.
#
# ZLIB_RS_SILESIA_URL comes from the environment.  Quoting it -- which this
# script does at every use -- stops the *shell* from splitting or globbing it,
# but says nothing about how curl and wget then parse it: to both of them an
# argument beginning with `-` is an option, not a URL.  That is not a
# theoretical concern.  With curl, a value of `-Kmy.conf` is taken as `-K`,
# curl reads that file as a configuration file, and every directive in it --
# `url`, `output`, `proxy`, `upload-file` -- is obeyed; a value of
# `-o/path/to/write` redirects the output.  wget offers the same shape through
# `-O`, `--output-document=` and `--config=`.
#
# Two independent measures close this off, and the script applies both:
#
#   1. `download_archive` passes `--` before the URL.  Both tools then treat
#      the following argument as an operand whatever it starts with, which is
#      the general fix and the one that does not depend on this function.
#   2. This function allowlists the scheme, which is the same check stated
#      positively: an argument that must begin with `https://` cannot begin
#      with `-`.
#
# `https://` is the whole accepted set, and this function agrees with
# `is_safe_url` deliberately rather than accidentally.  `is_safe_url` runs
# first, at the top of `main`, and has already refused anything else by the
# time execution reaches here; the check is repeated at this point because
# `probe_tools` has now run, so a diagnostic here can name the downloader that
# was actually selected.  Two checks, one contract: no `http://`, no `file://`,
# and no curl-only scheme such as scp://, sftp:// or smb://, whose credential
# and filesystem behaviour this script has not reasoned about and does not
# intend to offer.  Case is not folded: schemes are conventionally lower case,
# and folding would widen the accepted set for no benefit.
#
# This runs in the pre-flight block in `main`, before the first byte of network
# traffic and before any filesystem change, so a bad value costs nothing.
validate_url() {
    case "$ZLIB_RS_SILESIA_URL" in
        https://?*) ;;
        *)
            die "ZLIB_RS_SILESIA_URL must begin with https:// and name
       something after the scheme.  This does not:
           $ZLIB_RS_SILESIA_URL
       Any other value is refused rather than handed to $downloader: an
       argument that begins with '-' would be read as an option rather than
       as a URL, and one that begins with another scheme would take this
       fetch somewhere this script has not reasoned about.  If what you have
       is a local copy, install it into the destination directory yourself
       instead of running this script."
            ;;
    esac
}
download_archive() {
    info "Downloading $ZLIB_RS_SILESIA_URL"
    info "  via $downloader into the staging directory, https only"

    # `--` ends option parsing in both tools; see `validate_url` for why that
    # matters.  The output path needs no such guard: `normalise_destination`
    # makes the destination absolute and the staging directory is created
    # inside its parent, so "$1" always begins with '/'.
    info "  transfer ceiling ${max_archive_bytes} bytes, from ${bounds_source}"

    # ★ THE TRANSFER CEILING, enforced by the downloader itself.
    #
    # `--max-filesize` and `--quota` make the tool stop rather than this script
    # notice afterwards, which is the whole point: an integrity check that runs
    # after the disk is full has protected nothing (CWE-400).  Both act on the
    # advertised length where the server gives one and on the running total where
    # it does not, so a server that lies about `Content-Length` is still bounded.
    #
    # Neither option is a substitute for the digest, and neither is treated as one.
    # They bound the RESOURCE; `verify_archive` establishes the IDENTITY, and it
    # still runs, still compares the full 64 hex digits, and still refuses on any
    # mismatch.
    #
    # ★ THE REDIRECT IS PART OF THE TRANSPORT, AND VALIDATING THE URL DOES NOT
    # CONSTRAIN IT.  `validate_url` and `is_safe_url` only ever see the value the
    # caller supplied, so they establish that the FIRST request is https.  Both
    # tools follow redirects by default, and a 30x answer names the next URL --
    # so an https:// input could be answered with `Location: http://...` and the
    # 65 MiB would arrive in clear text, over a hop nothing in this script had
    # looked at.  The pinned SHA-256 would still detect tampering, which is
    # exactly the point worth being precise about: DETECTION AFTER THE FACT IS
    # NOT THE SAME AS NOT ACCEPTING THE DOWNGRADE.  A cleartext transfer is
    # observable and modifiable in flight whatever is checked afterwards, and a
    # fetch that has to be re-run because it was intercepted is a worse outcome
    # than one that refuses the hop.
    #
    # So the protocol restriction is stated to the tool, which is the only thing
    # that sees the redirect chain:
    #
    #   curl  --proto '=https' limits the protocols curl will use at all, and
    #         --proto-redir '=https' limits those it will follow a redirect TO.
    #         Both are needed: the first does not govern redirect targets.
    #         Measured: with a 302 to an http URL, curl 8.14 refuses with
    #         `Protocol "http" disabled (in redirect)' and writes no file.
    #   wget  is the fallback, and it CANNOT express "follow only https
    #         redirects" for a single download.  --https-only is passed and is
    #         worth passing -- it constrains the URL scheme and any recursive
    #         retrieval -- but measured against the same 302, GNU wget 1.25
    #         followed the downgrade and saved the cleartext body anyway,
    #         because that option governs recursive-mode link following.  So the
    #         redirect is refused OUTRIGHT here with --max-redirect=0: under
    #         wget this script does not follow redirects at all, rather than
    #         following one it has not been able to constrain.  The upstream
    #         archive answers 200 with no redirect (verified), so this costs the
    #         documented path nothing; a mirror that does redirect is handled by
    #         passing the final https URL in ZLIB_RS_SILESIA_URL, or by
    #         installing curl, which `probe_tools` already prefers.
    case "$downloader" in
        curl)
            curl -fSL --proto '=https' --proto-redir '=https' --retry 3 \
                 --max-filesize "$max_archive_bytes" \
                 -o "$1" -- "$ZLIB_RS_SILESIA_URL" ||
                die "download failed: $ZLIB_RS_SILESIA_URL
       curl was restricted to https for both the request and any redirect it
       was asked to follow (--proto '=https' --proto-redir '=https').  A
       redirect to plain http is refused here rather than accepted and
       checksummed afterwards, so if the archive has moved to an http host,
       find an https URL for it and pass it in ZLIB_RS_SILESIA_URL.

       It was also held to the ${max_archive_bytes}-byte transfer ceiling from
       ${bounds_source}.  A larger archive is not the corpus this repository
       approved; if upstream legitimately grew, re-pin ${PIN_NAME} rather than
       raising the ceiling on its own."
            ;;
        wget)
            wget --https-only --max-redirect=0 --quota="$max_archive_bytes" \
                 -O "$1" -- "$ZLIB_RS_SILESIA_URL" ||
                die "download failed: $ZLIB_RS_SILESIA_URL
       wget was restricted to https and to NO redirects at all
       (--https-only --max-redirect=0), because wget cannot be told to follow
       only https redirects for a single download -- so a redirect it could not
       constrain is refused instead of followed.  If the answer was a redirect,
       resolve it yourself and pass the final https URL:
           ZLIB_RS_SILESIA_URL=<final https url> $0 --sha256 <digest>
       or install curl, which this script prefers and which can restrict the
       redirect chain itself.

       It was also held to the ${max_archive_bytes}-byte transfer ceiling from
       ${bounds_source}.  See ${PIN_NAME}."
            ;;
        *)
            die "internal error: no downloader selected"
            ;;
    esac

    # A server that answers an error with a zero-length body, and wget's habit
    # of creating the output file before it discovers it cannot fetch the URL,
    # both land here.  Saying so plainly beats a digest mismatch on 0 bytes.
    if [ ! -s "$1" ]; then
        die "the download produced an empty file; check the URL:
       $ZLIB_RS_SILESIA_URL"
    fi

    # ★ And again on what actually landed, because `--quota` is advisory in one
    # documented case: wget applies it between files rather than mid-file, so a
    # single oversized file can exceed it.  Checking the result closes that gap for
    # both tools with one test, and it runs before the digest so an oversized
    # download is refused without hashing it.
    _got=$(file_size "$1")
    if [ -n "$_got" ] && [ "$_got" -gt "$max_archive_bytes" ]; then
        rm -f "$1"
        die "the download is ${_got} bytes, above the ${max_archive_bytes}-byte
       ceiling from ${bounds_source}; it has been deleted and nothing was
       extracted.  See ${PIN_NAME}."
    fi
    info "  received ${_got} bytes"
}

# The size of file $1 in bytes, or the empty string if it cannot be determined.
#
# `stat` is not POSIX and its two dialects take different flags, so both are tried
# and `wc -c` is the fallback that works everywhere. Returning empty rather than
# guessing keeps every caller's bound check explicit about the unknown case: a
# ceiling that silently passes because the size could not be read is worse than one
# that says so.
file_size() {
    if have stat; then
        stat -c '%s' "$1" 2>/dev/null && return 0
        stat -f '%z' "$1" 2>/dev/null && return 0
    fi
    if have wc; then
        wc -c < "$1" 2>/dev/null | tr -d ' \t'
        return 0
    fi
    printf ''
}

verify_archive() {
    info "Verifying SHA-256"
    compute_digest "$stage_dir" "$ARCHIVE_NAME"

    if [ "$computed_sha256" != "$expected_sha256" ]; then
        rm -f "$stage_dir/$ARCHIVE_NAME"
        die "SHA-256 mismatch -- the download was NOT installed.
         expected: $expected_sha256
           actual: $computed_sha256
       Source: $ZLIB_RS_SILESIA_URL
       Either the archive upstream has changed, in which case pin the new
       digest deliberately, or the transfer was corrupted or intercepted, in
       which case do not use these bytes."
    fi

    info "  ok: $computed_sha256"
}

# -----------------------------------------------------------------------------
# Archive-extraction safety.
#
# A digest match says the archive is the one we expected.  It says nothing about
# whether that archive is well behaved, and a zip member name is attacker-
# controlled data: it can be absolute, it can climb out with `..`, it can carry
# a Windows drive letter or backslash separators, and a member can be a symbolic
# link pointing anywhere on the filesystem.  Extracted naively, any of those
# writes outside the staging directory (CWE-22, path traversal) or plants a link
# that a later read or write follows somewhere it should not go (CWE-59).
#
# Neither supported unpacker can be relied on to stop this, which is why the
# checks below are ours and not theirs.  Measured on unzip 6.00 and bsdtar 3.7.7
# with purpose-built archives, the two tools disagree on every hostile case:
#
#   member                     unzip                    bsdtar
#   -----------------------    ----------------------   ----------------------
#   /abs/path                  warns, strips the /      warns, strips the /
#   ../../escape               SILENTLY flattens it     refuses, exit 1
#   ..\..\escape               creates that literal     treats \ as /, refuses
#                              name -- a traversal on
#                              a Windows host
#   C:/escape                  creates a `C:` directory strips the drive letter
#   symlink -> /etc/passwd     CREATES THE LINK         CREATES THE LINK
#
# So one tool silently rewrites what the other rejects, the exit statuses
# disagree, and *both* happily materialise an escaping symlink.  Depending on
# that would make the outcome a function of which tool the host happens to have.
#
# The rule enforced instead is deliberately stricter than "cannot escape": this
# corpus is twelve plain data files, so every member must be an ordinary
# relative path, and nothing but regular files and directories may result.  A
# legitimate Silesia archive passes; anything exotic is refused with the member
# named, and a refusal happens BEFORE a single byte is extracted.
# -----------------------------------------------------------------------------

# Print archive $1's member names, one per line.
#
# Both spellings are the extractor's own list-only mode and were checked to
# produce identical output for this archive: `unzip -Z1` is zipinfo mode with no
# header or footer, `bsdtar -tf` is the standard table of contents.  Directory
# members appear with a trailing slash in both, which the member validator
# handles.
list_archive_members() {
    case "$unpacker" in
        unzip) unzip -Z1 "$1" ;;
        bsdtar) bsdtar -tf "$1" ;;
        *) return 1 ;;
    esac
}

# Refuse the archive unless every member name is safe to extract.
#
# This runs BEFORE extraction, which is the whole point: a traversal that is
# merely detected afterwards has already written outside the staging directory.
#
# The listing is captured first and then read from a here-document rather than
# piped into the loop.  A pipeline puts the loop in a subshell, where `die` would
# terminate only that subshell and the script would sail on into the extraction
# it was supposed to prevent -- the same hazard the digest helpers above are
# written around.
assert_members_are_safe() {
    info "Checking the archive's member paths"

    _listing=$(list_archive_members "$1") ||
        die "could not list the members of the downloaded archive"

    [ -n "$_listing" ] ||
        die "the archive lists no members; it is not the expected corpus"

    _members=0
    while IFS= read -r _member; do
        # A blank line is not a member.  Skipping it rather than rejecting it
        # keeps the check tolerant of a trailing newline in the listing.
        [ -n "$_member" ] || continue

        _members=$((_members + 1))

        is_safe_member "$_member" || die "refusing to extract this archive: it
       contains an unsafe member path. Extracting it could write outside the
       staging directory.
         member: $_member
         Source: $ZLIB_RS_SILESIA_URL
       Do not use these bytes."
    done <<LISTING
$_listing
LISTING

    info "  ok: $_members members, every path safe"
}

# Refuse the extracted tree unless it holds only directories and plain files.
#
# The member-name preflight cannot cover this, because neither extractor reports
# member TYPES in a format that is stable across versions.  Inspecting the
# filesystem afterwards is both simpler and extractor-agnostic, and it catches
# anything the listing failed to reveal.
#
# `find` is used without `-L`/`-H`, so it does not follow symlinks and reports
# each link itself -- which is exactly what has to be detected.  The two
# conditions are separate passes because they differ in kind:
#
#   * anything that is neither a directory nor a regular file: symlinks first of
#     all, but also fifos (which would make a later read block forever), sockets
#     and device nodes (which would be written *through*).
#   * a regular file with more than one link: a hard link, which would let a
#     write inside the corpus reach a file outside it.  Restricted to `-type f`
#     because directories legitimately have a link count above one.
assert_tree_is_plain() {
    info "Checking the extracted tree for links and special files"

    _offenders=$(find "$1" ! -type d ! -type f -print 2>/dev/null | sed -n '1,5p')
    [ -z "$_offenders" ] || die "refusing to install this archive: the extracted
       tree contains entries that are neither directories nor regular files.
       A symbolic link here can redirect a later write to any path this user can
       reach.
$(printf '         %s\n' "$_offenders")
         Source: $ZLIB_RS_SILESIA_URL
       Do not use these bytes."

    _linked=$(find "$1" -type f -links +1 -print 2>/dev/null | sed -n '1,5p')
    [ -z "$_linked" ] || die "refusing to install this archive: the extracted
       tree contains hard-linked files, which can reach data outside it.
$(printf '         %s\n' "$_linked")
         Source: $ZLIB_RS_SILESIA_URL
       Do not use these bytes."

    info "  ok: directories and plain files only"
}

# -----------------------------------------------------------------------------
# ARCHIVE MEMBER PREFLIGHT.
#
# The digest pin proves *which* archive was fetched.  It proves nothing about
# whether that archive's member paths are safe to extract, and those are two
# different questions: an upstream repackaging, a compromised mirror, or a
# deliberately hostile ZLIB_RS_SILESIA_URL all produce an archive whose digest
# is whatever it is, and this script would then hand it to an extractor.  A zip
# can name a member `/etc/cron.d/x` or `../../../.ssh/authorized_keys`, and it
# can carry a symbolic link followed by a member that writes *through* it --
# the classic pair being `link -> /tmp` and then `link/payload` -- which lands
# outside the extraction directory without any `..` appearing in either name.
#
# Both extractors this script accepts do defend against the path cases by
# default: Info-ZIP unzip refuses `..` components unless `-:` is given, and
# bsdtar strips leading `/` and `..` unless `-P` is given.  Neither opt-out is
# ever passed here, so that default remains in force as a second layer.  It is
# deliberately not the argument, for three reasons: it is a property of the
# installed tool rather than of this script, it differs between
# implementations and versions, and it is *silent* -- a stripped path is a
# warning and an exit status of zero, so a hostile archive would install
# quietly.  Membership is therefore checked here, explicitly, and a dangerous
# archive is refused outright rather than partially extracted.
#
# On line-oriented listing.  Both listers were measured to escape a control
# character inside a member name rather than emit it raw (`unzip -Z1` renders
# an embedded newline as `^J`, `bsdtar -tf` as `\n`), so one member is exactly
# one line and a name cannot split itself across two.  The checks do not
# depend on that: were a lister ever to split a name, each fragment would
# still be tested, and a rejectable substring stays on whichever fragment it
# lands on -- so splitting can only ever produce more rejections, never fewer.
# -----------------------------------------------------------------------------

# Refuse member name $1 when extracting it could write outside the extraction
# directory.  Called from the current shell, never a subshell, so that `die`
# ends the run rather than a pipeline stage.
check_member_name() {
    case "$1" in
        /*)
            die "the archive contains a member whose path is absolute, so
       unpacking it could write outside the staging directory:
           $1
       Refusing to unpack this archive."
            ;;
        .. | ../* | */../* | */..)
            die "the archive contains a member with a '..' path component, so
       unpacking it could write outside the staging directory:
           $1
       Refusing to unpack this archive."
            ;;
        # Two shapes are deliberately *not* rejected here.  A name that merely
        # begins with two dots, such as `..keep`, is an ordinary filename and
        # not a traversal -- the patterns above require a '/' or an end of
        # string immediately after the '..' for exactly that reason.  And a
        # Windows-style `..\..\x` is a single filename on the systems this
        # script supports, since neither extractor translates a backslash into
        # a separator when extracting on POSIX, so it escapes nothing.
        *) ;;
    esac
}

# The one diagnostic for "the members could not be listed", named after the
# command that failed.  It deliberately does not assert *why*: measured on
# UnZip 6.00, `unzip -Z1` exits 1 on an empty archive and 9 on a file that is
# not a zip at all, while bsdtar exits 0 with no output for the first and 1 for
# the second.  Pinning either tool's status table, or parsing its text, would
# be a portability bet dressed up as precision, so the causes are listed
# instead.  All of them end the same way, which is the part that matters: an
# archive whose members cannot be inspected is not extracted.
listing_failed() {
    die "could not list the archive's members with '$1', so they cannot be
       checked before extraction.  Refusing to unpack this archive.
       It may be empty, truncated, or not a zip archive at all; if it is none
       of those then this build of $unpacker cannot list it, and the other
       supported extractor -- unzip with zipinfo support, or bsdtar from
       libarchive -- will be needed instead.
       Archive: $ZLIB_RS_SILESIA_URL"
}

# List the archive and refuse it if any member is unsafe.  Runs after the
# digest check, so only bytes that matched the pin are ever inspected, and
# before `mkdir`, so a refusal creates nothing.
preflight_archive_members() {
    info "Checking the archive's member paths"

    _archive="$stage_dir/$ARCHIVE_NAME"

    # Two listings per tool: names alone, which is the cleanest form to test
    # paths against, and the verbose form, whose leading mode string is the
    # only place the entry *type* is visible.  Their stderr is deliberately not
    # silenced -- when a lister rejects the file it explains itself far better
    # than this script could, and that explanation belongs in front of whoever
    # ran it.
    case "$unpacker" in
        unzip)
            _member_names=$(unzip -Z1 "$_archive") || listing_failed 'unzip -Z1'
            _member_modes=$(unzip -Z "$_archive") || listing_failed 'unzip -Z'
            ;;
        bsdtar)
            _member_names=$(bsdtar -tf "$_archive") || listing_failed 'bsdtar -tf'
            _member_modes=$(bsdtar -tvf "$_archive") || listing_failed 'bsdtar -tvf'
            ;;
        *)
            die "internal error: no unpacker selected"
            ;;
    esac

    # Reached only when the lister *succeeded* and still named nothing, which
    # is how bsdtar reports an empty archive.  unzip reports the same condition
    # by failing, and is caught above.
    if [ -z "$_member_names" ]; then
        die "the archive lists no members at all, so it is not the corpus:
       $ZLIB_RS_SILESIA_URL"
    fi

    # Here-documents rather than pipes throughout: a pipeline stage runs in a
    # subshell, where `die` would exit only that subshell and the run would
    # carry on into the extraction it was trying to prevent.
    _member_count=0
    while IFS= read -r _member; do
        if [ -n "$_member" ]; then
            check_member_name "$_member"
            _member_count=$((_member_count + 1))
        fi
    done <<MEMBER_NAMES
$_member_names
MEMBER_NAMES

    # The verbose listing puts a `ls`-style mode string first, so its initial
    # character is the entry type: `l` a symbolic link, `h` a hard link, `d` a
    # directory, `-` a regular file.  Neither link kind belongs in a corpus of
    # test data, and either can point outside the extraction directory, so both
    # are refused.  The whole line is quoted back because it carries the link
    # target as well as the name, which is what a reader needs to see.
    while IFS= read -r _line; do
        case "$_line" in
            l*)
                die "the archive contains a symbolic-link member, which could
       redirect a later member's contents outside the staging directory:
           $_line
       Refusing to unpack this archive."
                ;;
            h*)
                die "the archive contains a hard-link member, which could point
       at a file outside the staging directory:
           $_line
       Refusing to unpack this archive."
                ;;
            *) ;;
        esac
    done <<MEMBER_MODES
$_member_modes
MEMBER_MODES

    # ★ THE ARCHIVE-METADATA CEILINGS, all four, from the archive's own headers and
    # before a single byte is extracted.
    #
    # This is the CWE-400 half of this function, and it is separate from the path
    # checks above because it answers a different question: those ask "could a member
    # escape?", this asks "how much disk can this archive consume if I let it?". The
    # answer has to be computed from what the archive DECLARES, because by the time an
    # extraction has told you the truth it has already used the disk.
    #
    # The count bound uses the tally the loop above already produced. The size bounds
    # come from `member_sizes`, which reads the same listings.
    if [ "$_member_count" -gt "$max_member_count" ]; then
        die "the archive declares $_member_count members, above the ceiling of
       $max_member_count from ${bounds_source}.  The approved corpus has
       twelve.  Nothing has been extracted.  See ${PIN_NAME}."
    fi

    assert_declared_sizes_are_bounded "$_archive"

    info "  ok: $_member_count members, every path relative and contained"
    info "  ok: declared expanded size ${_declared_total} bytes, within \
${max_expanded_bytes}"
}

# ★ Bound the archive's DECLARED expanded size, per member and in total.
#
# Sets `_declared_total`. Reads the uncompressed size out of the verbose listing both
# unpackers already produce, so it costs no extra pass over the archive and no
# extraction.
#
# Three separate bounds, because each catches something the others do not:
#
#   * per-member, so one member claiming 400 MiB inside a total that happens to add
#     up is still refused;
#   * total, so a thousand small members are refused as surely as one big one;
#   * ratio of declared expansion to bytes actually downloaded, which is the bound
#     that catches a zip bomb whose per-member and total figures are individually
#     believable but whose expansion factor is not. The approved corpus expands 3.11x;
#     the ceiling is generous at 20x and still refuses the classic bomb, which
#     expands by six or more orders of magnitude.
#
# A listing this function cannot parse is NOT treated as a pass. The sizes are
# refused as unreadable and the run stops: a ceiling that silently does not apply is
# the failure mode this whole function exists to remove.
assert_declared_sizes_are_bounded() {
    _declared_total=0
    _declared_max=0
    _sized=0

    case "$unpacker" in
        # `unzip -Z1 -l` is not a listing this can use; the plain `unzip -Z` form
        # already in `_member_modes` puts the uncompressed size in field 4 of a
        # `ls -l`-shaped line. `unzip -l` puts it in field 1, which is easier to
        # parse and is what is used here.
        unzip) _size_listing=$(unzip -l "$_archive") || listing_failed 'unzip -l' ;;
        bsdtar) _size_listing=$_member_modes ;;
        *) die "internal error: no unpacker selected" ;;
    esac

    while IFS= read -r _line; do
        case "$unpacker" in
            unzip)
                # Data lines begin with whitespace then the size; the header, the
                # separator rules and the trailing total do not match this shape.
                _size=$(printf '%s\n' "$_line" |
                    sed -n 's/^ *\([0-9][0-9]*\)  *[0-9][0-9-]*-[0-9][0-9-]*.*$/\1/p')
                ;;
            bsdtar)
                # `bsdtar -tvf` is `ls -l`-shaped: mode, links, owner, group, size.
                _size=$(printf '%s\n' "$_line" |
                    sed -n 's/^[-dhl][rwxsStT-]*  *[0-9]*  *[^ ][^ ]*  *[^ ][^ ]*  *\([0-9][0-9]*\).*$/\1/p')
                ;;
        esac
        if [ -z "$_size" ]; then
            continue
        fi
        _sized=$((_sized + 1))
        if [ "$_size" -gt "$max_member_bytes" ]; then
            die "the archive declares a $_size-byte member, above the per-member
       ceiling of $max_member_bytes bytes from ${bounds_source}.  Nothing has
       been extracted.  See ${PIN_NAME}."
        fi
        if [ "$_size" -gt "$_declared_max" ]; then
            _declared_max=$_size
        fi
        _declared_total=$((_declared_total + _size))
        if [ "$_declared_total" -gt "$max_expanded_bytes" ]; then
            die "the archive declares at least $_declared_total expanded bytes,
       above the ceiling of $max_expanded_bytes from ${bounds_source}.
       Nothing has been extracted.  See ${PIN_NAME}."
        fi
    done <<SIZE_LISTING
$_size_listing
SIZE_LISTING

    if [ "$_sized" -eq 0 ]; then
        die "no member size could be read from the $unpacker listing, so the
       expanded-size ceiling could not be applied.  This is refused rather
       than skipped: an unenforced ceiling is worse than none, because the
       caller believes it applied.  Report the listing format."
    fi

    # The expansion-ratio bound. Integer arithmetic throughout -- there is no float
    # in POSIX shell and none is wanted here -- so the comparison is
    # declared > downloaded * ratio rather than a division.
    _archive_bytes=$(file_size "$_archive")
    if [ -n "$_archive_bytes" ] && [ "$_archive_bytes" -gt 0 ]; then
        if [ "$_declared_total" -gt \
            "$((_archive_bytes * SILESIA_MAX_EXPANSION_RATIO))" ]; then
            die "the archive is $_archive_bytes bytes and declares
       $_declared_total expanded bytes, an expansion of more than
       ${SILESIA_MAX_EXPANSION_RATIO}x.  The approved corpus expands about
       3.1x.  Nothing has been extracted."
        fi
    fi
}

# Refuse the extracted tree unless it is only regular files and directories.
# This is what closes CWE-59: both unpackers create symbolic links from an
# archive without complaint, and a link is exactly the thing that turns a
# contained extraction into an uncontained read or write later on.
assert_payload_safe() {
    _exotic=$(find "$1" ! -type f ! -type d -print 2> /dev/null) ||
        die "could not inspect the extracted tree at $1"

    if [ -n "$_exotic" ]; then
        die "the extracted tree contains entries that are neither regular files
       nor directories, so it was NOT installed:
$_exotic
       Symbolic links, hard links to outside the tree, device nodes and
       FIFOs have no place in a data corpus, and a link is followed by
       whatever reads the corpus next."
    fi

    # Anything that escaped one level up lands here, in the staging directory
    # beside the payload.  Nothing else is ever created there by this script,
    # so an unexpected name is by definition an escape.
    for _entry in "$stage_dir"/* "$stage_dir"/.[!.]*; do
        if [ ! -e "$_entry" ] && [ ! -L "$_entry" ]; then
            continue
        fi
        case "${_entry##*/}" in
            "$ARCHIVE_NAME" | payload | previous) ;;
            *)
                die "extraction created '${_entry##*/}' outside the payload
       directory, which means a member escaped.  Nothing was installed."
                ;;
        esac
    done
}

# Require the extracted payload to be exactly the expected corpus.
#
# `dir_is_empty` was the only previous check, and "not empty" is not the same as
# "the Silesia corpus".  This compares the sorted list of regular files, by
# basename, against SILESIA_MEMBERS.  Upstream ships the twelve members flat, so
# a basename comparison is both sufficient and tolerant of a mirror that wraps
# them in a directory.
#
# Set ZLIB_RS_SILESIA_MEMBERS to a space-separated list to accept a deliberately
# repackaged mirror; set it to `-` to skip the inventory check entirely, which is
# recorded as an explicit choice rather than a silent default.
#
# Called from TWO places, which is why $2 exists.  On the fetch path it judges a
# freshly extracted payload, and a mismatch is evidence about the ARCHIVE.  From
# `verify_only` it judges a payload installed by some earlier run, and a mismatch
# is evidence about THAT DIRECTORY -- the archive is long gone and is not what
# went wrong -- so the two need different advice.  Reading the same inventory in
# both places is the point: `--verify-only` promising to detect an "incomplete"
# destination while consulting only "is there at least one non-stamp entry" is
# how eleven of the twelve members, and even a single unrelated file, used to
# pass.
# The quiet predicate underneath it: 0 when the inventory under $1 is exactly
# ZLIB_RS_SILESIA_MEMBERS (or when the check is opted out), 1 otherwise.  It
# prints nothing and NEVER dies, which is what lets `destination_is_complete`
# call it -- that function is used as a condition, so an inventory check that
# exited would turn a decision into a termination.  It leaves the sorted lists in
# _found and _expected for whichever caller wants to report them.
member_inventory_matches() {
    if [ "$ZLIB_RS_SILESIA_MEMBERS" = '-' ]; then
        return 0
    fi

    _found=$(find "$1" -type f ! -name "$STAMP_NAME" -exec basename {} \; |
        sort) || return 1
    # Unquoted on purpose: the variable is a space-separated list and word
    # splitting is how it is read.
    # shellcheck disable=SC2086
    _expected=$(printf '%s\n' $ZLIB_RS_SILESIA_MEMBERS | sort)

    [ "$_found" = "$_expected" ]
}

verify_member_inventory() {
    # Silent on the opt-out path on purpose: `main` warns about it once, at the
    # entry point, so that the paths which never reach this function -- the
    # idempotent short-circuit and --verify-only, both of which decide through
    # `member_inventory_matches` -- are covered by the same one warning rather
    # than by none.  Warning again here would only make it twice for the fetch
    # path and still nothing for those two.
    if [ "$ZLIB_RS_SILESIA_MEMBERS" = '-' ]; then
        return 0
    fi

    if [ "${2-}" = installed ]; then
        info "Checking the installed member inventory"
    else
        info "Checking the extracted member inventory"
    fi

    if ! member_inventory_matches "$1"; then
        printf 'expected:\n%s\nfound:\n%s\n' "$_expected" "$_found" >&2
        if [ "${2-}" = installed ]; then
            die "the payload installed in
         $1
       is not the expected corpus inventory (see above), so this destination is
       incomplete or is holding something else.  The archive it was fetched
       from is not retained, so nothing here can repair it: replace the
       directory with --force, or -- if you are deliberately benchmarking a
       repackaged mirror -- set ZLIB_RS_SILESIA_MEMBERS to the list you mean."
        fi
        die "the extracted payload is not the expected corpus inventory (see
       above).  Either the archive at
         $ZLIB_RS_SILESIA_URL
       is not Silesia, or upstream has repackaged it.  If the latter, set
       ZLIB_RS_SILESIA_MEMBERS to the new list deliberately."
    fi

    # shellcheck disable=SC2086
    info "  ok: all $(printf '%s\n' $ZLIB_RS_SILESIA_MEMBERS | wc -l |
        tr -d ' ') expected members are present and nothing else is"
}

# Verify each member of the tree at $1 against a digest manifest.
#
# This is the check that upgrades "these bytes arrived intact" to "these bytes
# are the corpus somebody I trust described".  On a FETCH it is optional and
# supplementary: the archive was hashed against a pinned digest moments earlier,
# so the members are already established, and a manifest merely adds evidence
# that survives upstream repackaging the archive.  On --verify-only it is the
# ONLY byte evidence obtainable at all -- the archive is not retained, so its
# digest can never be recomputed -- which is why that mode requires it.
#
# ZLIB_RS_SILESIA_MEMBER_SHA256 names a file in `sha256sum` format
# ("<64 hex>  <name>", two spaces).  Only the basename of each entry is used, so
# a manifest written against a different directory layout still applies.  A value
# of `-` is the deliberate, visible waiver, spelled the same way as the one
# ZLIB_RS_SILESIA_MEMBERS accepts.
#
# $2, when it is the word `complete', additionally requires the manifest to cover
# every member of the expected inventory.  Callers pass it when this check is the
# only one standing: a manifest listing one of the twelve authenticates one of
# the twelve, and reporting that as "verified" would be the same overstatement
# this function exists to remove.  A fetch does not pass it, because there the
# archive digest already covers everything the manifest does not.
verify_member_digests() {
    if [ "$ZLIB_RS_SILESIA_MEMBER_SHA256" = '-' ]; then
        warn "per-member digest check waived by request"
        warn "(ZLIB_RS_SILESIA_MEMBER_SHA256=-), so nothing below is a statement"
        warn "about the corpus BYTES"
        return 0
    fi

    if [ -z "$ZLIB_RS_SILESIA_MEMBER_SHA256" ]; then
        return 0
    fi

    _manifest="$ZLIB_RS_SILESIA_MEMBER_SHA256"
    [ -f "$_manifest" ] ||
        die "ZLIB_RS_SILESIA_MEMBER_SHA256 does not name a readable file:
       '$_manifest'"

    # --verify-only reaches this without having probed for a downloader or an
    # extractor it will never use, so the one tool this needs is probed here.
    probe_digest_tool

    info "Checking per-member digests against $_manifest"

    # Counted, and the names collected, so that "ok" cannot be printed over a
    # manifest that authenticated nothing.  An empty file, or one holding only
    # comments, used to reach the closing `info' unchallenged.
    _checked=0
    _checked_names=''

    while IFS= read -r _line; do
        case "$_line" in
            '' | '#'*) continue ;;
        esac

        _want=${_line%% *}
        _entry=${_line##* }
        _entry=${_entry##*/}

        if ! is_sha256 "$(lowercase "$_want")"; then
            die "malformed line in $_manifest (expected '<64 hex>  <name>'):
       '$_line'"
        fi

        _path=$(find "$1" -type f -name "$_entry" -print | head -n 1)
        [ -n "$_path" ] ||
            die "$_manifest lists '$_entry', which
         $1
       does not contain"

        compute_digest "$(dirname "$_path")" "$(basename "$_path")"
        if [ "$computed_sha256" != "$(lowercase "$_want")" ]; then
            die "member '$_entry' does not match $_manifest
         expected: $(lowercase "$_want")
         actual:   $computed_sha256
       These are not the bytes the manifest describes.  Do not measure with
       them: replace the corpus with --force, or find out why they differ."
        fi

        _checked=$((_checked + 1))
        _checked_names="$_checked_names$_entry
"
    done < "$_manifest"

    if [ "$_checked" -eq 0 ]; then
        die "$_manifest lists no digests at all (every line is blank or a
       comment), so it authenticates nothing.  A manifest that cannot fail is
       not a check: supply the real one, or set
       ZLIB_RS_SILESIA_MEMBER_SHA256=- to waive this deliberately."
    fi

    # Coverage, when the caller says this check stands alone.  Compared name by
    # name rather than by count, so a manifest listing `dickens' twice cannot
    # stand in for a manifest listing `dickens' and `mozilla'.
    if [ "${2-}" = complete ] && [ "$ZLIB_RS_SILESIA_MEMBERS" != '-' ]; then
        _missing=''
        # Unquoted on purpose: the variable is a space-separated list and word
        # splitting is how a `for' reads one.  No suppression is needed here,
        # unlike the `printf' uses of the same variable elsewhere, because
        # ShellCheck does not object to splitting in this position.
        for _member in $ZLIB_RS_SILESIA_MEMBERS; do
            case "
$_checked_names" in
                *"
$_member
"*) ;;
                *) _missing="$_missing $_member" ;;
            esac
        done

        if [ -n "$_missing" ]; then
            die "$_manifest does not cover the whole expected inventory, and here
       it is the only evidence about the corpus bytes, so a partial manifest
       would report more than it checked.
         unlisted:$_missing
       Either extend the manifest to every member named by
       ZLIB_RS_SILESIA_MEMBERS, or waive the check deliberately with
       ZLIB_RS_SILESIA_MEMBER_SHA256=-."
        fi
    fi

    info "  ok: $_checked member(s) match the digests in the manifest"
}

unpack_archive() {
    # Two independent pre-extraction layers, both running before a single byte is
    # written: the first validates every member path against the extractor's own
    # listing, the second re-derives that listing and re-checks it with a
    # different predicate.  Neither delegates to the extractor's own sanitising,
    # because both supported extractors sanitise *silently* and still exit zero.
    preflight_archive_members
    assert_members_are_safe "$stage_dir/$ARCHIVE_NAME"

    info "Unpacking with $unpacker"

    mkdir -p "$1" || die "could not create $1"

    case "$unpacker" in
        unzip)
            # -q keeps the log short; -o keeps it non-interactive, since unzip
            # otherwise prompts when a member already exists.
            unzip -q -o -d "$1" "$stage_dir/$ARCHIVE_NAME" ||
                die "unzip failed on the downloaded archive"
            ;;
        bsdtar)
            # No -P: bsdtar's default is to refuse absolute paths and `..`,
            # which is a second line of defence behind the preflight above.
            bsdtar -x -f "$stage_dir/$ARCHIVE_NAME" -C "$1" ||
                die "bsdtar failed on the downloaded archive"
            ;;
        *)
            die "internal error: no unpacker selected"
            ;;
    esac

    if dir_is_empty "$1"; then
        die "the archive unpacked to nothing; it is not the expected corpus"
    fi

    # Confirm that what landed on disk is what the listing described: nothing
    # but directories and regular files.  This does not replace the preflight
    # and cannot -- extraction is a single pass, so a link that slipped past the
    # listing would already have been written through -- but it is the cheapest
    # available check that the lister enumerated the archive honestly, and it
    # runs while the tree is still in staging, so a refusal installs nothing.
    _unexpected=$(find "$1" ! -type d ! -type f) ||
        die "could not inspect the unpacked tree in $1"

    if [ -n "$_unexpected" ]; then
        die "the unpacked tree contains entries that are neither directories nor
       regular files, which a corpus of test data must not:
$_unexpected
       Refusing to install it."
    fi

    # Re-walk what was actually created, before anything stamps or installs it.
    assert_tree_is_plain "$1"
    assert_payload_safe "$1"

    # And confirm the payload is the corpus rather than merely *a* safe tree: the
    # expected inventory, plus per-member digests when a manifest was supplied.
    verify_member_inventory "$1"
    verify_member_digests "$1"
}

# Move the staged tree $1 into the destination, replacing an existing directory
# only after it has been moved aside, and putting it back if the install fails.
install_payload() {
    if [ -e "$destination" ]; then
        # Recorded BEFORE the move, not after.  `cleanup` tests this path for
        # existence, so naming it early is harmless if the move never happens,
        # and it removes the instant in which the data would be displaced but
        # unrecorded -- an interruption there used to mean `cleanup` deleted the
        # staging directory with the caller's corpus inside it.
        previous_dir="$stage_dir/previous"

        mv "$destination" "$previous_dir" || {
            previous_dir=''
            die "could not move the existing $destination aside"
        }
    fi

    if ! mv "$1" "$destination"; then
        if [ -n "$previous_dir" ] && [ -e "$previous_dir" ]; then
            if mv "$previous_dir" "$destination"; then
                previous_dir=''
                warn "install failed; the previous contents were restored"
            else
                # Suppress the staging cleanup so the displaced data survives.
                preserve_stage=1
                warn "install failed and the previous contents could not be
         restored automatically; they have been left in $previous_dir"
            fi
        fi
        die "could not install the corpus into $destination"
    fi

    # The install is committed: the destination now holds the new corpus, so the
    # displaced copy is genuinely obsolete and `cleanup` should remove it with
    # the rest of the staging directory.  Clearing this is what says so -- and it
    # must happen only here, after the rename succeeded.
    previous_dir=''
}

# Silent predicate: the destination holds a complete fetch that this script
# installed.  Deliberately quiet, so the default run can consult it without
# emitting anything.
destination_is_complete() {
    [ -d "$destination" ] || return 1
    read_stamp "$destination" || return 1
    payload_present "$destination" || return 1
    # COMPLETE means the pinned inventory, not "something is in there".  Without
    # this line the function's name was a promise it did not keep: `payload_present`
    # is satisfied by a single non-stamp entry, so a destination holding eleven of
    # the twelve members short-circuited a fetch with "there is nothing to do"
    # while --verify-only, reading the same directory, called it incomplete.  One
    # definition of complete, used by both.
    member_inventory_matches "$destination" || return 1
    return 0
}

# Summarise an installation whose stamp has just been read.
report_existing() {
    info "Silesia corpus present at: $destination"
    info "  archive: $stamp_url"
    info "  sha256:  $stamp_sha256"
}

# -----------------------------------------------------------------------------
# Cross-check an existing installation against what is expected NOW.
#
# The stamp is this script's own note to itself, written into a directory that
# anybody can edit and that any earlier run may have written with different
# settings.  Treating its presence as proof would mean a file the script does
# not authenticate decides whether verification happens at all: hand-write a
# well-formed stamp, or leave a real one behind after changing the URL, and an
# unverified directory is reported as a verified corpus (CWE-345).
#
# So the stamp is evidence of provenance, never of authenticity.  Both recorded
# fields must equal what is expected on this run before any caller may treat the
# directory as good, and `resolve_expected_digest` must already have run -- which
# is why every caller resolves the expected values FIRST and only then looks at
# what is on disk.  That ordering is the fix: a check that runs after an early
# success return is not a check.
#
# Requires `expected_sha256` to be set; refuses to guess if it is not.
# -----------------------------------------------------------------------------
assert_stamp_matches_expected() {
    if [ -z "$expected_sha256" ]; then
        die "internal error: the expected digest was not resolved before the
       installed corpus was checked against it"
    fi

    if [ "$stamp_url" != "$ZLIB_RS_SILESIA_URL" ]; then
        die "the installed corpus came from a different archive than the one
       expected now.
         expected: $ZLIB_RS_SILESIA_URL
         recorded: $stamp_url
       The URL and the digest identify one archive jointly, so a corpus
       fetched from elsewhere is not interchangeable with this one.  Pass
       --force to replace it, or point ZLIB_RS_SILESIA_URL back."
    fi

    if [ "$stamp_sha256" != "$expected_sha256" ]; then
        die "the installed corpus was not produced by the archive that is
       expected now.
         expected: $expected_sha256   (from $expected_sha256_source)
         recorded: $stamp_sha256
       Pass --force to refetch and verify against the expected digest if
       that is the archive you now want."
    fi
}

# The --verify-only entry point.  Reports what is on disk and exits non-zero
# for anything it cannot vouch for; fetches nothing and writes nothing.
#
# The expected URL and digest are resolved before the directory is examined, so
# there is no order of events in which this reports success without having
# compared both against the stamp.  Supplying a digest is therefore mandatory
# here too: verifying against nothing is not verification, it is trusting a file
# that the attacker being defended against could have written.
#
# AND THE SAME ARGUMENT APPLIES ONE LEVEL DOWN, WHICH IS WHY A MEMBER MANIFEST IS
# REQUIRED HERE.  The stamp records the URL and the archive digest of the fetch
# that produced this directory -- but the stamp is a plain file INSIDE that
# directory, and so is every corpus member, and so are their names.  Everything
# compared above therefore comes from the artifact being vouched for, which makes
# a clean report on a tampered, half-restored or stale directory entirely
# possible: rewrite a member and the stamp still matches, because the archive
# those bytes came from is not retained and its digest can never be recomputed
# here.  The one piece of evidence that can come from outside is a per-member
# digest manifest, so this mode demands one -- or an explicit `-' waiver, which
# is recorded in the report so that a weaker check is never mistaken for the
# stronger one.  A fetch needs no manifest: it hashes the archive it actually
# downloaded against a pinned digest before unpacking a byte of it.
verify_only() {
    # Idempotent, and `main` has already done it: repeated here so the ordering
    # guarantee belongs to this function rather than to its caller.
    resolve_expected_digest

    if [ ! -e "$destination" ]; then
        die "nothing to verify: $destination does not exist.
       Run this script with no arguments to fetch the corpus first."
    fi

    if [ ! -d "$destination" ]; then
        die "$destination exists but is not a directory"
    fi

    if ! read_stamp "$destination"; then
        die "$destination carries no readable, well-formed $STAMP_NAME, so this
       script cannot vouch for what is in it.  Replace it with --force."
    fi

    if ! payload_present "$destination"; then
        die "$destination is stamped but holds no corpus files, so the fetch that
       produced it did not complete.  Replace it with --force."
    fi

    # "Present" is not "complete", and the two used to be conflated here: the
    # check above is satisfied by ONE non-stamp entry, so a destination holding
    # eleven of the twelve members -- or one unrelated file -- reported success
    # while --help promised a non-zero exit for an incomplete one.  The inventory
    # is the same one the fetch path applies, read with `find` and `basename`
    # only, so this stays a mode that writes nothing and reaches no network.
    verify_member_inventory "$destination" installed

    # Demanded before anything is reported, so that the refusal names the missing
    # input rather than arriving after a page of reassuring output.
    if [ -z "$ZLIB_RS_SILESIA_MEMBER_SHA256" ]; then
        die "--verify-only cannot say anything about the corpus BYTES without
       ZLIB_RS_SILESIA_MEMBER_SHA256, and everything else it can check --
       the stamp, the file names, the inventory -- lives inside the very
       directory it is being asked to vouch for.  The archive is not retained
       after a fetch, so its digest cannot be recomputed here either.  So:
         * point ZLIB_RS_SILESIA_MEMBER_SHA256 at a sha256sum-format manifest
           covering every member, which is the check that authenticates them
           against evidence from outside this directory; or
         * set ZLIB_RS_SILESIA_MEMBER_SHA256=- to accept a stamp-and-inventory
           report as a deliberate, visible choice.
       A fetch needs neither: it verifies the archive it downloaded against
       the expected digest before unpacking it."
    fi

    report_existing
    info "  stamp:   well formed, and the payload is the complete pinned inventory"

    assert_stamp_matches_expected
    info "  archive: matches the expected URL"
    info "  digest:  matches the expected archive digest"

    # The bytes.  `complete' because here this is the only evidence there is, so
    # a manifest covering part of the inventory must not report as if it covered
    # all of it.
    verify_member_digests "$destination" complete

    # Said plainly rather than implied, and said differently in the two cases,
    # because the difference between them is the whole point of the requirement
    # above.
    if [ "$ZLIB_RS_SILESIA_MEMBER_SHA256" = '-' ]; then
        info "Verified AS FAR AS THE STAMP GOES.  The archive is not retained"
        info "after a fetch, and the per-member digest check was waived, so this"
        info "confirms the recorded fetch and the inventory -- both read from"
        info "inside this directory -- and nothing about the corpus bytes."
    else
        info "Verified, bytes included.  The archive is not retained after a"
        info "fetch, so its digest was not recomputed; every member was hashed"
        info "and matched against the manifest instead, which is evidence from"
        info "outside this directory."
    fi
}

# ★ Report the pin and the ceilings, and exit.  Reads nothing else and writes nothing.
#
# The output is `key=value` on stdout, one per line, deliberately the same shape as
# the pin file itself, so that `.github/workflows/rust.yml` can read it with `grep`
# and a shell can read it with `eval`-free `read`. Anything advisory goes to stderr
# through `info`, so stdout stays machine-readable.
#
# `armed=yes|no` is the field CI acts on: it says whether this repository carries an
# approved corpus identity at all. That is the difference between "the acceptance
# measurement was not taken" and "the acceptance measurement cannot be taken here",
# and before the pin file existed there was no way to tell them apart.
pin_status() {
    if load_pin; then
        resolve_bounds
        printf 'armed=yes\n'
        printf 'pin=%s\n' "$pin_path"
        printf 'url=%s\n' "$pin_url"
        printf 'sha256=%s\n' "$pin_sha256"
        printf 'archive_bytes=%s\n' "$pin_archive_bytes"
        printf 'member_count=%s\n' "$pin_member_count"
        printf 'expanded_bytes=%s\n' "$pin_expanded_bytes"
        printf 'max_member_bytes=%s\n' "$pin_max_member_bytes"
        printf 'compression_ratio=%s\n' "$pin_compression_ratio"
    else
        resolve_bounds
        printf 'armed=no\n'
        printf 'pin=\n'
    fi
    printf 'enforced_archive_bytes=%s\n' "$max_archive_bytes"
    printf 'enforced_expanded_bytes=%s\n' "$max_expanded_bytes"
    printf 'enforced_member_bytes=%s\n' "$max_member_bytes"
    printf 'enforced_member_count=%s\n' "$max_member_count"
    printf 'bounds_source=%s\n' "$bounds_source"

    if [ -f "${script_dir}/${MANIFEST_NAME}" ]; then
        printf 'manifest=%s\n' "${script_dir}/${MANIFEST_NAME}"
    else
        printf 'manifest=\n'
    fi
}

main() {
    parse_arguments "$@"

    # ★ Before anything else, and before the URL is even looked at: --pin-status
    # answers a question about this repository rather than about a corpus, so it must
    # not be able to fail on a destination, a tool or a URL.
    if [ "$opt_pin_status" -eq 1 ]; then
        pin_status
        return 0
    fi

    # The pin supplies tier 3 of the digest chain and all four resource ceilings, so
    # it is read before `resolve_expected_digest` consults that chain and before any
    # path that downloads or extracts consults a bound. A missing pin is tolerated
    # here -- `resolve_expected_digest` will refuse the run if no tier supplied a
    # digest, and `resolve_bounds` falls back to the built-in ceilings -- so that
    # `--verify-only` against a pre-provisioned directory keeps working in a fork
    # that carries no pin.
    load_pin || true
    resolve_bounds

    # The committed per-member manifest is the default rather than something a
    # caller has to remember to point at. An explicit --manifest still wins, and a
    # fork that deleted the file gets the same behaviour as before: no per-member
    # check.
    if [ -z "$ZLIB_RS_SILESIA_MEMBER_SHA256" ] &&
        [ -f "${script_dir}/${MANIFEST_NAME}" ]; then
        ZLIB_RS_SILESIA_MEMBER_SHA256="${script_dir}/${MANIFEST_NAME}"
        info "using the committed per-member manifest ${MANIFEST_NAME}"
    fi

    # The download URL is validated here, before anything else looks at it.
    # ZLIB_RS_SILESIA_URL is caller-supplied and reaches a command line, the
    # stamp file and several diagnostics, so it is checked once at the entry
    # point rather than at each of those places -- and ahead of --verify-only
    # too, which compares it against the URL recorded in an existing stamp.
    is_safe_url "$ZLIB_RS_SILESIA_URL" || die "ZLIB_RS_SILESIA_URL is not an
       acceptable download URL. It must begin with https:// and contain no
       whitespace or control characters.
         value: $ZLIB_RS_SILESIA_URL"

    # THE INVENTORY-BYPASS WARNING BELONGS HERE, ONCE, FOR EVERY MODE.  It used
    # to live only in `verify_member_inventory`, which three of this script's
    # paths never reach: `--verify-only` on a stamped directory and the
    # idempotent "there is nothing to do" short-circuit both decide through
    # `destination_is_complete`, which consults the quiet predicate
    # `member_inventory_matches` instead -- and that returns 0 without a word
    # when the check is opted out.  So a run with the check disabled could report
    # success having verified nothing about WHICH files are present, while the
    # documentation promised a warning.  Announcing it at the entry point covers
    # every path, including the ones that exit before any inventory is examined,
    # and it is emitted exactly once because nothing downstream repeats it.
    if [ "$ZLIB_RS_SILESIA_MEMBERS" = '-' ]; then
        warn "inventory check disabled by request (ZLIB_RS_SILESIA_MEMBERS=-):"
        warn "  no path in this run will check WHICH files the payload holds, so"
        warn "  \"complete\" here means stamped and non-empty and nothing more."
    fi

    resolve_destination

    # WHAT WE EXPECT IS ESTABLISHED BEFORE WHAT IS ON DISK IS LOOKED AT.
    #
    # This one line's position is load-bearing.  Resolving the expected digest
    # here, rather than just before the download as it once was, is what makes
    # the idempotent short-circuit below safe: every path that reports success
    # has an expected URL and digest in hand and has compared both against the
    # stamp.  Move it back down and a hand-written or stale stamp is once again
    # accepted without question, because the comparison would have nothing to
    # compare against yet.
    resolve_expected_digest

    if [ "$opt_verify_only" -eq 1 ]; then
        verify_only
        return 0
    fi

    if [ -e "$destination" ] && [ ! -d "$destination" ]; then
        die "$destination exists and is not a directory; refusing to touch it"
    fi

    if [ -d "$destination" ] && ! dir_is_empty "$destination"; then
        if destination_is_complete; then
            if [ "$opt_force" -eq 0 ]; then
                # Stamped is not the same as trusted: the recorded URL and
                # digest must both be the ones expected now, or this is some
                # other corpus and skipping the fetch would be wrong.
                assert_stamp_matches_expected
                # And when a manifest is available, the members are hashed on
                # this path too.  Nothing is downloaded here, so the stamp is
                # exactly as self-referential as it is under --verify-only; the
                # manifest is not REQUIRED here only because this invocation is
                # able to refetch, which --verify-only by definition is not.
                verify_member_digests "$destination"
                report_existing
                info "Already fetched from the expected archive and verified"
                info "against the expected digest, so there is nothing to do."
                info "Pass --force to fetch it again."
                return 0
            fi
            info "--force given: replacing the corpus at $destination"
        else
            if [ "$opt_force" -eq 0 ]; then
                die "$destination already exists but is not a complete corpus this
       script installed: it carries no valid $STAMP_NAME, or its payload is
       missing, or its contents are not the inventory named by
       ZLIB_RS_SILESIA_MEMBERS.  Nothing has been touched.  Re-run with --force
       to replace it, remove it yourself, or point ZLIB_RS_SILESIA_DIR somewhere
       else.  --verify-only reports which of those it is."
            fi
            warn "replacing the existing, unstamped $destination (--force)"
        fi
    fi

    # Everything that can fail without a byte of network traffic or a single
    # filesystem change fails first: a missing tool, an unusable URL, and an
    # unpinned digest.  `validate_url` comes after `probe_tools` so that it can
    # name the downloader that was actually selected.
    probe_tools
    validate_url
    resolve_expected_digest

    install_traps
    stage_create

    download_archive "$stage_dir/$ARCHIVE_NAME"
    verify_archive
    unpack_archive "$stage_dir/payload"
    write_stamp "$stage_dir/payload"
    install_payload "$stage_dir/payload"

    info ""
    info "Silesia corpus ready at: $destination"
    info "The benchmarks discover it automatically -- measure with:"
    if [ "$destination_is_default" -eq 1 ]; then
        info "    cargo bench --manifest-path benches/Cargo.toml"
    else
        info "    ZLIB_RS_SILESIA_DIR=$destination \\"
        info "        cargo bench --manifest-path benches/Cargo.toml"
    fi
    # Precisely, because "no CI job" was written here and is not true.  No
    # automated path FETCHES this corpus: no test, no build script, and no CI
    # job.  Reading a corpus provisioned some other way is a different matter --
    # the benchmark suites read this directory, and the `bench' job of
    # .github/workflows/rust.yml runs this script against it in --verify-only,
    # the one mode that neither fetches nor writes.
    info "No test and no build script reads it, and no automated path fetches"
    info "it.  The benchmarks read it, and CI asks about a corpus provisioned"
    info "elsewhere with --verify-only."
}

main "$@"
