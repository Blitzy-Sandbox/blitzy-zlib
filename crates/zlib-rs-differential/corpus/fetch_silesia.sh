#!/bin/sh
# fetch_silesia.sh -- materialise the Silesia compression corpus so that the
#                     repository-root benchmark suites have realistic input.
#
# =============================================================================
# OPT-IN ONLY.  A HUMAN RUNS THIS SCRIPT, DELIBERATELY.  NOTHING ELSE DOES.
# =============================================================================
#
# This is the only networked artifact anywhere in this crate, and the most
# important thing about it is what it does *not* do:
#
#   * it is NEVER invoked by `cargo test`;
#   * it is NEVER invoked by CI;
#   * it is referenced from no `Cargo.toml`, from no `build.rs`, from no file
#     under `tests/`, and from no workflow in `.github/workflows/`;
#   * it has no side effect of any kind -- not a directory, not a temporary
#     file, not a single byte of network traffic -- unless a human runs it.
#
# Those are not aspirations, they are the property that keeps this crate's
# test suite hermetic.  The correctness gates here -- the byte-identity
# matrix, the round-trip interoperability tests and the table-equality checks
# -- must be reproducible from a clone alone, so `cargo test` must never touch
# the network.  The moment any automated path calls this script, every gate
# becomes contingent on a remote host that may be unreachable, may be slow, or
# may serve something different today than it served yesterday.  If you are
# adding automation and find yourself wanting this corpus, the answer is to
# make the automation skip, exactly as the benchmarks do.
#
# WHY IT EXISTS
#
# Solely so that `cargo bench` has large, heterogeneous, realistic input.  The
# suites at the repository root -- benches/deflate_bench.rs and
# benches/inflate_bench.rs, attached to this crate by the `[[bench]]` entries
# in ../Cargo.toml -- measure throughput against the in-tree C reference, and
# throughput only means something when it is measured on data of realistic
# size and variety.  Silesia is the standard corpus for exactly that.  This is
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
# Silesia is also third-party data, and it is deliberately not redistributed
# by this repository.  Whoever runs this script accepts the corpus's own
# upstream terms; they are not restated here, because the copy that matters is
# the one upstream publishes at fetch time.
#
# See ./README.md for the full rationale, the tier-1 fixture inventory, and
# the path contract this script implements.  Where this script and that README
# disagree about a path, the README is the published contract and this script
# is wrong.
#
# -----------------------------------------------------------------------------
# CHECKSUM POLICY -- PLEASE READ BEFORE REPORTING A FAILED RUN
# -----------------------------------------------------------------------------
#
# Every download is verified against an expected SHA-256, and that check
# cannot be waived: there is no flag to skip it, no environment variable that
# disables it, and a mismatch is always fatal.
#
# Upstream publishes the archive but does not publish a digest for it.  The
# constant below is therefore left at the sentinel value shown, and this
# script FAILS CLOSED -- it refuses to fetch anything at all -- until a
# maintainer pins the real digest.  That is a deliberate choice between three
# options, and it is the only honest one: accepting an unverified download
# would defeat the purpose of the check, and writing in a digest computed from
# a single unauthenticated download would look authoritative while being
# nothing more sound than trust-on-first-use.
#
# To pin it, once, on a machine you trust:
#
#   1. Fetch the archive by hand:
#        curl -fSL -o silesia.zip \
#            'https://sun.aei.polsl.pl/~sdeor/corpus/silesia.zip'
#   2. Compute its digest:
#        sha256sum silesia.zip            # or: shasum -a 256 silesia.zip
#   3. Put the 64-character result in SILESIA_SHA256_EXPECTED below -- one
#      place, one line, visible in a diff and reviewable -- or supply it per
#      invocation instead:
#        ZLIB_RS_SILESIA_SHA256=<digest> ./fetch_silesia.sh
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
# Portability: POSIX shell only, matching the in-tree ./configure.  No
# bashisms, no `local`, no arrays, no `readlink -f`, no process substitution.
# It adds no crate to the workspace, no entry to any manifest, and no tool
# beyond the ubiquitous utilities probed for below.

set -eu

# -----------------------------------------------------------------------------
# Configuration.  Every one of these is overridable from the environment; the
# `${VAR:-default}` form means an empty value is treated as unset, which is
# what makes `ZLIB_RS_SILESIA_DIR=` behave like "use the default" rather than
# like "install into the filesystem root".
# -----------------------------------------------------------------------------

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
# variable: override it to point at a mirror or at a local copy (`file://...`,
# which curl accepts) rather than editing this script.
ZLIB_RS_SILESIA_URL=${ZLIB_RS_SILESIA_URL:-https://sun.aei.polsl.pl/~sdeor/corpus/silesia.zip}

# The sentinel that means "no digest has been pinned yet".  It is deliberately
# not a valid SHA-256, so it can never be confused with one.
SILESIA_SHA256_SENTINEL='UNPINNED'

# THE ONE PLACE A MAINTAINER PINS THE DIGEST.  See the CHECKSUM POLICY block
# above.  While this reads UNPINNED the script fetches nothing.
SILESIA_SHA256_EXPECTED="$SILESIA_SHA256_SENTINEL"

# Per-invocation override of the expected digest.  Case-insensitive; it must
# be exactly 64 hexadecimal characters or the run is refused.
ZLIB_RS_SILESIA_SHA256=${ZLIB_RS_SILESIA_SHA256:-$SILESIA_SHA256_EXPECTED}

# The marker this script writes inside the destination once a fetch has
# completed and verified.  Its presence -- not a guess about which member
# files the archive happens to contain -- is what makes a re-run idempotent,
# so upstream repackaging cannot confuse the check.  It is dot-prefixed to
# mark it as metadata rather than corpus data.
STAMP_NAME='.fetch_silesia.stamp'

# Basename used for the archive while it sits in the staging directory.  Kept
# free of shell- and sed-special characters on purpose: the digest is computed
# from inside the staging directory using this literal name, so no quirk of a
# user-supplied destination path can perturb the digest tool's output format.
ARCHIVE_NAME='silesia.zip'

# Selected by `probe_tools`; declared here so the whole configuration surface
# is visible in one place.
downloader=''
digest_tool=''
unpacker=''

# Set by the argument parser.
opt_force=0
opt_verify_only=0

# Set by `resolve_destination`: the directory the corpus is installed into, and
# whether that came from the default rather than from the environment.
destination=''
destination_is_default=0

# Set by `read_stamp` from the marker inside an existing installation.
stamp_url=''
stamp_sha256=''

# Set by `stage_create`; consulted by `cleanup`.
stage_dir=''

# Set only when an install failed AND the displaced previous contents could not
# be put back, which is the single case in which `cleanup` must keep the staging
# directory rather than delete it.
preserve_stage=0

# -----------------------------------------------------------------------------
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
# -----------------------------------------------------------------------------
script_dir=$(cd "$(dirname "$0")" && pwd)
repo_root=$(cd "$script_dir/../../.." && pwd)

# -----------------------------------------------------------------------------
# Helpers.
# -----------------------------------------------------------------------------

# Progress, on stdout.  Everything this script says about what it is doing goes
# through here so that a caller can silence it with a single redirection.
info() {
    printf '%s\n' "$*"
}

# Non-fatal problem, on stderr.
warn() {
    printf 'fetch_silesia.sh: warning: %s\n' "$*" >&2
}

# Fatal problem: diagnostic on stderr, non-zero exit.  Every failure path in
# this script ends here, so no failure can ever exit 0.
die() {
    printf 'fetch_silesia.sh: error: %s\n' "$*" >&2
    exit 1
}

# Is a tool available?  `command -v` is the POSIX spelling; `which` is not.
have() {
    command -v "$1" >/dev/null 2>&1
}

# Lowercase a string, so that a digest supplied in upper case compares equal.
lowercase() {
    printf '%s' "$1" | tr '[:upper:]' '[:lower:]'
}

# True when directory $1 contains no entries at all, dotfiles included.  An
# unmatched POSIX glob expands to itself, hence the existence test on each
# candidate rather than a count.
dir_is_empty() {
    for _entry in "$1"/* "$1"/.[!.]*; do
        if [ -e "$_entry" ] || [ -L "$_entry" ]; then
            return 1
        fi
    done
    return 0
}

# True when directory $1 contains at least one entry that is not the stamp,
# i.e. when it holds actual corpus data.
payload_present() {
    for _entry in "$1"/* "$1"/.[!.]*; do
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

# -----------------------------------------------------------------------------
# Usage.  Written to stdout for `--help` so it can be piped; the argument-error
# path redirects it to stderr instead.
# -----------------------------------------------------------------------------
usage() {
    cat <<'EOF'
Usage: fetch_silesia.sh [--force] [--verify-only] [-h|--help]

Fetches the Silesia compression corpus for the repository-root benchmark
suites.  OPT-IN ONLY: nothing in this repository runs this script for you, and
no correctness gate depends on what it produces.  Running it is never required
-- `cargo test` does not need it, and `cargo bench` reports and skips when the
corpus is absent.

Options:
  --force        Re-fetch even when the destination is already populated,
                 replacing whatever is there.  Also required to overwrite a
                 populated destination that carries no stamp from a previous
                 successful run.
  --verify-only  Report on an existing download and exit without fetching
                 anything.  Exits non-zero when the destination is missing,
                 incomplete, or carries an unreadable stamp.  The archive
                 itself is not retained after a fetch, so this re-checks the
                 recorded stamp and the presence of the payload; it does not
                 recompute a digest over the corpus files.
  -h, --help     Print this text and exit 0.  Touches no network and creates
                 no files.

Environment:
  ZLIB_RS_SILESIA_DIR      Destination directory.  Unset or empty selects
                           <repo-root>/target/silesia, which the root
                           .gitignore already ignores.  The benchmarks read
                           this same variable, so set it for them too.
  ZLIB_RS_SILESIA_URL      Source archive URL.  Override to use a mirror or a
                           local copy.
  ZLIB_RS_SILESIA_SHA256   Expected SHA-256 of the archive, 64 hex digits,
                           case-insensitive.  Required unless a maintainer has
                           pinned SILESIA_SHA256_EXPECTED in this script: the
                           checksum is never optional and never skipped.

Space: roughly 65 MiB downloaded and a few hundred megabytes unpacked; allow
about 300 MiB free in the destination's filesystem.

Examples:
  ./crates/zlib-rs-differential/corpus/fetch_silesia.sh
  ZLIB_RS_SILESIA_DIR=/var/cache/silesia \
      ./crates/zlib-rs-differential/corpus/fetch_silesia.sh
  ZLIB_RS_SILESIA_DIR=/var/cache/silesia cargo bench -p zlib-rs-differential

See ./README.md for the corpus contract this script implements.
EOF
}

# -----------------------------------------------------------------------------
# Argument parsing.  Unknown arguments are refused rather than ignored: a typo
# in a flag must not silently turn into a default-configuration fetch.
# -----------------------------------------------------------------------------
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

# -----------------------------------------------------------------------------
# Tool probing.  Each family degrades to a sensible alternative and otherwise
# fails closed with a message naming what to install.  All of it happens before
# any network access or filesystem mutation, so a machine missing a tool learns
# that immediately instead of after a 65 MiB download.
# -----------------------------------------------------------------------------
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

# -----------------------------------------------------------------------------
# Digest handling.
#
# These set globals rather than printing their result, because a `die` inside a
# command substitution would only terminate the subshell.  Failing loudly and
# exactly once, from the shell that can actually exit, is worth the global.
# -----------------------------------------------------------------------------

# Normalised expected digest, set by `resolve_expected_digest`.
expected_sha256=''

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

# True when an expected digest has actually been pinned or supplied.
digest_is_pinned() {
    [ "$ZLIB_RS_SILESIA_SHA256" != "$SILESIA_SHA256_SENTINEL" ]
}

# Normalise and validate the expected digest into `expected_sha256`, failing
# closed when it has not been pinned.  There is no path through this function
# that leaves verification disabled.
resolve_expected_digest() {
    if ! digest_is_pinned; then
        die "the expected SHA-256 of the archive has not been pinned, so this
       fetch is refused.  An unverified corpus is not acceptable and there
       is deliberately no flag to skip verification.

       Supply it for this run:
           ZLIB_RS_SILESIA_SHA256=<64-hex-digest> $0
       or pin it once in SILESIA_SHA256_EXPECTED near the top of this
       script.  To obtain it, on a machine you trust:
           curl -fSL -o silesia.zip '$ZLIB_RS_SILESIA_URL'
           sha256sum silesia.zip        # or: shasum -a 256 silesia.zip
       The CHECKSUM POLICY block in this script explains why it is not
       pinned already."
    fi

    expected_sha256=$(lowercase "$ZLIB_RS_SILESIA_SHA256")

    if ! is_sha256 "$expected_sha256"; then
        die "the expected SHA-256 is not a 64-character hexadecimal digest:
       '$ZLIB_RS_SILESIA_SHA256'"
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

# -----------------------------------------------------------------------------
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
# -----------------------------------------------------------------------------
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

# -----------------------------------------------------------------------------
# The stamp: this script's own record that a fetch completed and verified.
# -----------------------------------------------------------------------------

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

# -----------------------------------------------------------------------------
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
# -----------------------------------------------------------------------------
cleanup() {
    if [ -z "$stage_dir" ] || [ ! -d "$stage_dir" ]; then
        return 0
    fi

    # One case must not be cleaned up: an install that both failed and could
    # not put the caller's previous corpus back. Deleting the staging directory
    # then would destroy data this script displaced, so it is kept and its
    # location reported instead. Everything else goes.
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

download_archive() {
    info "Downloading $ZLIB_RS_SILESIA_URL"
    info "  via $downloader into the staging directory"

    case "$downloader" in
        curl)
            curl -fSL --retry 3 -o "$1" "$ZLIB_RS_SILESIA_URL" ||
                die "download failed: $ZLIB_RS_SILESIA_URL"
            ;;
        wget)
            wget -O "$1" "$ZLIB_RS_SILESIA_URL" ||
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

unpack_archive() {
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
            bsdtar -x -f "$stage_dir/$ARCHIVE_NAME" -C "$1" ||
                die "bsdtar failed on the downloaded archive"
            ;;
        *)
            die "internal error: no unpacker selected"
            ;;
    esac

    # Whatever the archive's internal layout is, it is preserved verbatim; the
    # only requirement is that unpacking produced something.
    if dir_is_empty "$1"; then
        die "the archive unpacked to nothing; it is not the expected corpus"
    fi
}

# Move the staged tree $1 into the destination, replacing an existing directory
# only after it has been moved aside, and putting it back if the install fails.
install_payload() {
    _previous=''

    if [ -e "$destination" ]; then
        _previous="$stage_dir/previous"
        mv "$destination" "$_previous" ||
            die "could not move the existing $destination aside"
    fi

    if ! mv "$1" "$destination"; then
        if [ -n "$_previous" ] && [ -e "$_previous" ]; then
            if mv "$_previous" "$destination"; then
                warn "install failed; the previous contents were restored"
            else
                # Suppress the staging cleanup so the displaced data survives.
                preserve_stage=1
                warn "install failed and the previous contents could not be
         restored automatically; they have been left in $_previous"
            fi
        fi
        die "could not install the corpus into $destination"
    fi
}

# -----------------------------------------------------------------------------
# Reporting on what is already installed.
# -----------------------------------------------------------------------------

# Silent predicate: the destination holds a complete fetch that this script
# installed.  Deliberately quiet, so the default run can consult it without
# emitting anything.
destination_is_complete() {
    [ -d "$destination" ] || return 1
    read_stamp "$destination" || return 1
    payload_present "$destination" || return 1
    return 0
}

# Summarise an installation whose stamp has just been read.
report_existing() {
    info "Silesia corpus present at: $destination"
    info "  archive: $stamp_url"
    info "  sha256:  $stamp_sha256"
}

# The --verify-only entry point.  Reports what is on disk and exits non-zero
# for anything it cannot vouch for; fetches nothing and writes nothing.
verify_only() {
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

    report_existing
    info "  stamp:   well formed, and the payload is present"

    if digest_is_pinned; then
        resolve_expected_digest
        if [ "$stamp_sha256" != "$expected_sha256" ]; then
            die "the installed corpus was not produced by the archive that is
       expected now.
         expected: $expected_sha256
         recorded: $stamp_sha256
       Replace it with --force if the new digest is the one you want."
        fi
        info "  digest:  matches the expected archive digest"
    else
        info "  digest:  no expected digest is pinned, so the recorded one was"
        info "           not cross-checked (see the CHECKSUM POLICY block)"
    fi

    # Said plainly rather than implied: the archive is not kept after a fetch,
    # so this confirms the recorded fetch and the presence of its payload, not
    # the bytes of the individual corpus files.
    info "Verified.  Note that the archive is not retained after a fetch, so"
    info "this checks the recorded fetch rather than re-hashing the corpus."
}

# -----------------------------------------------------------------------------
# Main.
# -----------------------------------------------------------------------------
main() {
    parse_arguments "$@"
    resolve_destination

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
                report_existing
                info "Already fetched and stamped, so there is nothing to do."
                info "Pass --force to fetch it again."
                return 0
            fi
            info "--force given: replacing the corpus at $destination"
        else
            if [ "$opt_force" -eq 0 ]; then
                die "$destination already exists but is not a corpus this script
       installed: it carries no valid $STAMP_NAME, or its payload is missing.
       Nothing has been touched.  Re-run with --force to replace it, remove
       it yourself, or point ZLIB_RS_SILESIA_DIR somewhere else."
            fi
            warn "replacing the existing, unstamped $destination (--force)"
        fi
    fi

    # Everything that can fail without a byte of network traffic or a single
    # filesystem change fails first: a missing tool, and an unpinned digest.
    probe_tools
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
        info "    cargo bench -p zlib-rs-differential"
    else
        info "    ZLIB_RS_SILESIA_DIR=$destination \\"
        info "        cargo bench -p zlib-rs-differential"
    fi
    info "Nothing else reads it: no test, no build script, and no CI job."
}

main "$@"

