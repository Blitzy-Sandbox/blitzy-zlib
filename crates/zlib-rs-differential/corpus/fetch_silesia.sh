#!/bin/sh
# fetch_silesia.sh -- materialise the Silesia compression corpus so that the
#                     repository-root benchmark suites have realistic input.
#
# This is the only networked artifact anywhere in this crate, and the most
# important thing about it is what it does *not* do:
#
#   * it is NEVER invoked by `cargo test`;
#   * it NEVER FETCHES from CI -- see the exact boundary below;
#   * it is referenced from no `Cargo.toml`, from no `build.rs` and from no file
#     under `tests/`;
#   * it has no side effect of any kind -- not a directory, not a temporary
#     file, not a single byte of network traffic -- unless a human runs it
#     WITHOUT `--verify-only`.
#
# THE EXACT CI BOUNDARY, because "never invoked by CI" used to be written here
# and was not true.  The `bench` job of `.github/workflows/rust.yml` invokes this
# script in exactly one mode, `--verify-only`, which "fetches nothing and writes
# nothing" (see `verify_only` below) and therefore reaches no network and touches
# no path.  It NEVER invokes it in fetching mode, and the workflow fails closed
# when the corpus it was told to expect is absent rather than downloading it:
# provisioning is done from outside the workflow, by a human or by a cache entry
# keyed on the pinned digest.  That is what AAP 0.6.4.4 asks for -- the corpus is
# opt-in "so `cargo test` and CI never require the network" -- and a side-effect
# free verification does not require one.
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
# with `--verify-only`, which is the one mode that neither fetches nor writes.
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
# THE DIGEST IS UNPINNED, SO EVERY INVOCATION MUST SUPPLY ONE.  Upstream
# publishes the archive but no digest for it, so SILESIA_SHA256_EXPECTED below is
# left at the sentinel `UNPINNED` and this script FAILS CLOSED -- it refuses to
# fetch anything at all -- until a digest arrives.  A bare
# `./fetch_silesia.sh` therefore does not download anything; it exits non-zero
# and tells you this.  That is deliberate: accepting an unverified download would
# defeat the purpose of the check, and writing in a digest computed from a single
# unauthenticated download would look authoritative while being nothing sounder
# than trust-on-first-use.
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
#   2. An archive digest obtained from such a source and pinned below.
#   3. An archive digest computed from your own download: corruption detection
#      only.  Fine for a throughput measurement on a machine you control, and
#      it must not be mistaken for more than that.
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
#      either pin it in SILESIA_SHA256_EXPECTED below -- one place, one line,
#      visible in a diff and reviewable -- or supply it on every invocation:
#        ZLIB_RS_SILESIA_SHA256=<64-hex-digest> ./fetch_silesia.sh
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

# THE ONE PLACE A MAINTAINER PINS THE DIGEST -- tier 3, the lowest-precedence
# of the three sources, used when neither --sha256 nor the environment supplies
# one.  See the CHECKSUM POLICY block above for why it does not ship pinned.
#
# Pin it only together with the ZLIB_RS_SILESIA_URL default above: a digest
# without the URL it was taken from asserts nothing.  While this reads UNPINNED
# a digest must come from --sha256 or from the environment, and without either
# the script fetches nothing and vouches for nothing.
SILESIA_SHA256_EXPECTED="$SILESIA_SHA256_SENTINEL"

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

# Optional path to a `sha256sum`-format manifest of PER-MEMBER digests.  When
# supplied, every listed member is hashed after extraction and must match.
#
# This is the check that makes the download authenticated rather than merely
# intact: the archive digest below can only ever be trust-on-first-use unless it
# reaches you through a channel independent of the download itself, whereas a
# member manifest obtained from a paper, a distribution package or a colleague's
# machine is independent evidence, and it keeps working when upstream rebuilds
# the zip.  Unset by default, because this repository has no authenticated source
# for those digests to ship.
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
Usage: fetch_silesia.sh --sha256 <digest> [--force] [--verify-only]
       fetch_silesia.sh [-h|--help]

Fetches the Silesia compression corpus for the repository-root benchmark
suites.  OPT-IN ONLY: nothing in this repository runs this script for you, and
no correctness gate depends on what it produces.  Running it is never required
-- `cargo test` does not need it, and `cargo bench` reports and skips when the
corpus is absent.

An expected SHA-256 is REQUIRED on every run, including --verify-only.  It has
to be supplied because upstream publishes no digest for this archive and zip
archives are not reproducible, so there is no single correct value to ship.
See the CHECKSUM POLICY block at the top of this script for the full reasoning
and for how to obtain a digest you can trust.

Options:
  --sha256 <hex> Expected SHA-256 of the archive: exactly 64 hex digits,
                 case-insensitive.  Outranks ZLIB_RS_SILESIA_SHA256 and the
                 SILESIA_SHA256_EXPECTED constant.  There is no way to skip
                 the check and no value that disables it.
  --force        Re-fetch even when the destination is already populated,
                 replacing whatever is there.  Also required to overwrite a
                 populated destination that carries no stamp from a previous
                 successful run.  It does not weaken verification: the fresh
                 download is checked exactly as any other.
  --verify-only  Report on an existing download and exit without fetching
                 anything.  Exits non-zero when the destination is missing,
                 incomplete, carries an unreadable stamp, or was fetched from
                 a URL or digest other than the ones expected now.
                 "Incomplete" is judged against ZLIB_RS_SILESIA_MEMBERS, the
                 same inventory a fetch applies, so a destination holding
                 eleven of the twelve members fails here.  The archive is not
                 retained after a fetch, so this re-checks the recorded stamp
                 against what you expect and confirms the inventory is
                 complete; it does not recompute a digest over the unpacked
                 corpus files.
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
                           Optional path to a sha256sum-format manifest of
                           per-member digests.  When set, every listed member is
                           hashed after extraction and must match.  This is the
                           only check here that can authenticate rather than
                           merely detect corruption.

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
  ZLIB_RS_SILESIA_DIR=/var/cache/silesia \
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

# Tool probing.  Each family degrades to a sensible alternative and otherwise
# fails closed with a message naming what to install.  All of it happens before
# any network access or filesystem mutation, so a machine missing a tool learns
# that immediately instead of after a 65 MiB download.
probe_tools() {
    if have curl; then
        downloader='curl'
    elif have wget; then
        downloader='wget'
    else
        die "no downloader found; install curl (preferred) or wget"
    fi

    # sha256sum is coreutils; shasum ships with macOS, which has no sha256sum.
    if have sha256sum; then
        digest_tool='sha256sum'
    elif have shasum; then
        digest_tool='shasum'
    else
        die "no SHA-256 tool found; install sha256sum (coreutils) or shasum"
    fi

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
# pinned SHA-256 afterwards, so plain http is not a confidentiality or integrity
# hole in itself, but there is no reason to accept a downgrade, and refusing
# `file://` and every other scheme keeps this from becoming a way to make the
# script read an arbitrary local path.
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
    info "  via $downloader into the staging directory"

    # `--` ends option parsing in both tools; see `validate_url` for why that
    # matters.  The output path needs no such guard: `normalise_destination`
    # makes the destination absolute and the staging directory is created
    # inside its parent, so "$1" always begins with '/'.
    case "$downloader" in
        curl)
            curl -fSL --retry 3 -o "$1" -- "$ZLIB_RS_SILESIA_URL" ||
                die "download failed: $ZLIB_RS_SILESIA_URL"
            ;;
        wget)
            wget -O "$1" -- "$ZLIB_RS_SILESIA_URL" ||
                die "download failed: $ZLIB_RS_SILESIA_URL"
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

    info "  ok: $_member_count members, every path relative and contained"
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
    if [ "$ZLIB_RS_SILESIA_MEMBERS" = '-' ]; then
        warn "inventory check skipped by request (ZLIB_RS_SILESIA_MEMBERS=-)"
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

# Verify each extracted member against a digest manifest, when one is supplied.
#
# This is the check that upgrades the archive digest from "these bytes arrived
# intact" to "these bytes are the corpus somebody I trust described".  It is
# optional because this repository cannot ship digests it has no authenticated
# source for -- see the CHECKSUM POLICY block -- but when a manifest IS available
# it is strictly better than the archive digest alone, because it survives
# upstream repackaging the archive.
#
# ZLIB_RS_SILESIA_MEMBER_SHA256 names a file in `sha256sum` format
# ("<64 hex>  <name>", two spaces).  Only the basename of each entry is used, so
# a manifest written against a different directory layout still applies.
verify_member_digests() {
    if [ -z "$ZLIB_RS_SILESIA_MEMBER_SHA256" ]; then
        return 0
    fi

    _manifest="$ZLIB_RS_SILESIA_MEMBER_SHA256"
    [ -f "$_manifest" ] ||
        die "ZLIB_RS_SILESIA_MEMBER_SHA256 does not name a readable file:
       '$_manifest'"

    info "Checking per-member digests against $_manifest"

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
            die "$_manifest lists '$_entry', which the archive did not contain"

        compute_digest "$(dirname "$_path")" "$(basename "$_path")"
        if [ "$computed_sha256" != "$(lowercase "$_want")" ]; then
            die "member '$_entry' does not match $_manifest
         expected: $(lowercase "$_want")
         actual:   $computed_sha256"
        fi
    done < "$_manifest"

    info "  ok: every member listed in the manifest matches"
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

    report_existing
    info "  stamp:   well formed, and the payload is the complete pinned inventory"

    assert_stamp_matches_expected
    info "  archive: matches the expected URL"
    info "  digest:  matches the expected archive digest"

    # Said plainly rather than implied: the archive is not kept after a fetch, so
    # this confirms the recorded fetch and the completeness of its inventory, not
    # the bytes of the individual corpus files.
    info "Verified.  Note that the archive is not retained after a fetch, so"
    info "this checks the recorded fetch and the inventory rather than"
    info "re-hashing the corpus."
}

main() {
    parse_arguments "$@"

    # The download URL is validated here, before anything else looks at it.
    # ZLIB_RS_SILESIA_URL is caller-supplied and reaches a command line, the
    # stamp file and several diagnostics, so it is checked once at the entry
    # point rather than at each of those places -- and ahead of --verify-only
    # too, which compares it against the URL recorded in an existing stamp.
    is_safe_url "$ZLIB_RS_SILESIA_URL" || die "ZLIB_RS_SILESIA_URL is not an
       acceptable download URL. It must begin with https:// and contain no
       whitespace or control characters.
         value: $ZLIB_RS_SILESIA_URL"

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
    info "Nothing else reads it: no test, no build script, and no CI job."
}

main "$@"
