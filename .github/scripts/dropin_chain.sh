#!/bin/sh
# dropin_chain.sh -- verify the shared-library symlink chain a Rust `make rust'
# staged, exactly as it was staged, and prove the verification can fail.
#
# WHY THIS EXISTS.  A drop-in replacement for libz is not a file, it is a chain.
# The library declares `SONAME libz.so.1', so at load time the dynamic loader
# searches for a file literally called libz.so.1; if the link directory holds
# only libz.so, the loader falls through to /lib/<triple>/libz.so.1 and the
# consumer runs happily against the SYSTEM zlib while -L and -rpath both point
# at the Rust build.  Every test passes and nothing under test was exercised.
# AAP 0.3.1.2 records that being reproduced, and AAP 0.8.3 makes reproducing the
# chain -- libz.so.<version> with libz.so.1 and libz.so pointing at it -- an
# installation requirement rather than a convenience.
#
# WHAT THIS DOES DIFFERENTLY FROM WHAT IT REPLACED.  The drop-in CI job used to
# `cp -L' the staged libz.so, which DEREFERENCES it, and then `ln -sf' fresh
# libz.so.1 and libz.so.<version> aliases beside the copy.  That is a repair:
# a producer that staged a dangling alias, an absolute link target, a flattened
# regular file where a link belongs, or no alias at all still yielded a green
# job, because the consumer never saw what the producer made.  Nothing here
# creates, retargets or dereferences a link.  Every name is read from the
# producer's own descriptor, every link is asserted by its literal target text
# AND by its canonicalised destination, and a chain that does not hold is a
# failure of this script rather than something it papers over.
#
# THE DESCRIPTOR IS THE PRODUCER'S, NOT A GUESS.  Makefile.in's `rust' target
# writes $(RUSTLIBDIR)/soname.stage with three lines -- the real versioned file,
# the major alias that becomes the SONAME, and the version-script mode -- and
# every later target reads it rather than re-deriving the names.  This script
# does the same, so a producer that renames its artifact cannot leave the
# verifier checking a name nobody staged.  `--expect-version' additionally
# cross-checks the staged name against ZLIB_VERSION from the immutable zlib.h,
# which is the one place the two independent derivations must agree.
#
# USAGE
#   dropin_chain.sh verify <staged-dir> [--expect-version <ver>]
#       Assert the chain in <staged-dir>.  Prints an evidence block naming every
#       path, its type, its literal link target, its canonical destination and
#       the SONAME the loader will search for.  Exit 0 when the chain holds.
#
#   dropin_chain.sh self-test <staged-dir>
#       Copy <staged-dir> with `cp -a', assert the copy PASSES (which is what
#       proves an archive copy preserves the chain), then perturb the copy one
#       way at a time and assert every perturbation FAILS.  Exit 0 when the
#       verifier accepted the good tree and rejected all of the broken ones.
#
# Portable POSIX sh: no bashisms, because this runs from `make' recipes as well
# as from CI.

set -eu

ME=dropin_chain.sh

die() {
    printf '%s: %s\n' "$ME" "$*" >&2
    exit 1
}

# `::error::' is GitHub Actions' annotation prefix; harmless anywhere else.
fail() {
    printf '::error::%s: %s\n' "$ME" "$*" >&2
    exit 1
}

# The one place a name is turned into a path, so that a name carrying a
# directory component cannot escape the staged directory.
plain_name() {
    case $1 in
        */* | '' | . | ..) return 1 ;;
        *) return 0 ;;
    esac
}

# ---------------------------------------------------------------------------
# verify
# ---------------------------------------------------------------------------
verify() {
    dir=$1
    expect_version=$2

    [ -d "$dir" ] || fail "$dir is not a directory; nothing was staged"

    stage=$dir/soname.stage
    [ -f "$stage" ] || fail "$stage is missing, so the producer's own record of what \
it staged is unavailable.  Run \`make rust' first; this script deliberately does \
not guess the names."

    # Three lines, in the order Makefile.in writes them.  Read with sed rather
    # than `read' so that a short file is diagnosed here instead of yielding an
    # empty name that then fails as a missing file.
    sov=$(sed -n 1p "$stage")
    som=$(sed -n 2p "$stage")
    vsmode=$(sed -n 3p "$stage")
    lines=$(wc -l < "$stage" | tr -d ' ')

    [ "$lines" -ge 3 ] || fail "$stage has $lines line(s); it must have at least \
three -- the versioned file, the major alias and the version-script mode."
    [ -n "$sov" ] || fail "$stage line 1 (the versioned file name) is empty"
    [ -n "$som" ] || fail "$stage line 2 (the major alias) is empty"
    [ -n "$vsmode" ] || fail "$stage line 3 (the version-script mode) is empty"

    plain_name "$sov" || fail "the staged versioned name '$sov' is not a plain file \
name; a descriptor carrying a path component is not something to resolve"
    plain_name "$som" || fail "the staged major alias '$som' is not a plain file name"

    # `required' and `ldshared' are the same fact recorded from two producers.
    # Makefile.in composes the shared-object link itself and writes `required'
    # when it passes --version-script on that command line; when the version
    # script is ALREADY in $(LDSHARED) -- which is exactly what ./configure
    # writes, so it is the state of every configured tree -- it has nothing to
    # add and records `ldshared' instead.  Either way a version script WAS
    # applied, which is the only thing that matters to this script: it is why the
    # symbol version nodes below are expected to exist.  Makefile.in's own
    # consumers already treat the two identically (each tests `= none' or
    # `= static'), and rejecting `ldshared' here made this verifier fail on the
    # documented `./configure; make; make rust' path while passing on the
    # unconfigured one -- a disagreement between two parts of this repository,
    # not a fault in the tree being verified.
    case $vsmode in
        required | ldshared | none) ;;
        static)
            fail "the producer recorded a static-only build, so there is no shared \
object to verify.  This gate needs the shared chain; build without \
ZLIB_RS_SHARED=0."
            ;;
        *) fail "unknown version-script mode '$vsmode' in $stage" ;;
    esac

    [ "$sov" != "$som" ] || fail "the versioned file and the major alias are both \
named '$sov'; the chain needs two distinct names"
    [ "$sov" != libz.so ] || fail "the versioned file is named libz.so, so there is \
no versioned name for the aliases to point at"

    # --- the real file: a regular file, NOT a link ------------------------
    real=$dir/$sov
    [ -e "$real" ] || fail "$real is absent, although $stage names it as the \
versioned library the aliases point at"
    [ ! -L "$real" ] || fail "$real is a symlink.  It must be the real file: an \
installation copies it and points the aliases at it, and a link here means the \
real object lives somewhere this tree does not control."
    [ -f "$real" ] || fail "$real is not a regular file"

    real_canonical=$(readlink -e "$real") \
        || fail "$real could not be canonicalised"

    # --- the archive ------------------------------------------------------
    archive=$dir/libz.a
    [ -f "$archive" ] || fail "$archive is absent; infcover.c links the static \
archive because inflate_table is hidden in both implementations"
    [ ! -L "$archive" ] || fail "$archive is a symlink; it must be the real archive"

    # --- SONAME: what the loader will actually search for -----------------
    soname=
    if command -v readelf > /dev/null 2>&1; then
        soname=$(readelf -d "$real" 2>/dev/null \
            | sed -n 's/.*SONAME.*\[\(.*\)\].*/\1/p' | sed -n 1p)
        [ -n "$soname" ] || fail "$real declares no SONAME.  Without one the loader \
searches for the file name the consumer was linked against and the versioned \
aliases buy nothing."
        [ "$soname" = "$som" ] || fail "$real declares SONAME '$soname' but $stage \
names '$som' as the major alias.  The loader will search for '$soname', so the \
alias the producer staged is not the one that gets used -- which is exactly how a \
consumer silently binds the system zlib."
    fi

    # --- the two aliases: links, with the target text and destination both
    # --- asserted.  `readlink' gives the literal target and `readlink -e' the
    # --- canonical destination.  The two are not redundant: the text assertion is
    # --- the stricter of the pair and catches a dangling or retargeted alias
    # --- first, while the canonical one is what notices an alias that names the
    # --- right file yet resolves outside this tree -- a staged directory reached
    # --- through a symlinked path, or a target that only looks relative.
    for alias in libz.so "$som"; do
        path=$dir/$alias
        [ "$alias" != "$sov" ] || continue
        [ -L "$path" ] || {
            if [ -e "$path" ]; then
                fail "$path exists but is not a symlink.  A regular file here is a \
DEREFERENCED copy -- the producer's chain was flattened rather than preserved, \
which is the defect this gate exists to catch."
            fi
            fail "$path is absent.  The loader searches for '$som' by SONAME, so \
without it a consumer linked against this directory falls through to the system \
zlib and passes while exercising nothing."
        }

        target=$(readlink "$path")
        [ "$target" = "$sov" ] || fail "$path points at '$target'; it must point at \
'$sov' by that exact name.  A path-bearing or absolute target does not survive \
being installed or copied, and pointing at anything else means the chain no \
longer converges on the object that was built."

        canonical=$(readlink -e "$path") \
            || fail "$path is a DANGLING symlink (it names '$target', which does \
not resolve).  The loader treats that as absent and falls through to the system \
zlib."
        [ "$canonical" = "$real_canonical" ] || fail "$path canonicalises to \
'$canonical' rather than '$real_canonical'.  The alias resolves outside this \
staged tree, so what a consumer loads is not what this tree contains."
    done

    # --- optional cross-check against the immutable header ----------------
    if [ -n "$expect_version" ]; then
        case $sov in
            "libz.so.$expect_version" | "libz.$expect_version.dylib") ;;
            *) fail "the producer staged '$sov', but ZLIB_VERSION is \
'$expect_version', so the versioned name should be \
'libz.so.$expect_version'.  Two independent derivations of the same name \
disagreeing means one of them is reading a stale value." ;;
        esac
    fi

    # --- evidence ---------------------------------------------------------
    printf '%s: chain verified in %s\n' "$ME" "$dir"
    printf '  %-28s regular file, %s bytes\n' "$sov" "$(wc -c < "$real" | tr -d ' ')"
    for alias in libz.so "$som"; do
        [ "$alias" != "$sov" ] || continue
        printf '  %-28s symlink -> %-24s canonical %s\n' \
            "$alias" "$(readlink "$dir/$alias")" "$(readlink -e "$dir/$alias")"
    done
    printf '  %-28s regular file, %s bytes\n' libz.a \
        "$(wc -c < "$archive" | tr -d ' ')"
    printf '  %-28s %s\n' 'SONAME (searched for)' "${soname:-<readelf unavailable>}"
    printf '  %-28s %s\n' 'version-script mode' "$vsmode"
}

# ---------------------------------------------------------------------------
# self-test
# ---------------------------------------------------------------------------
# Every case starts from an archive copy of the REAL staged tree and differs
# from it by exactly one mutation, so a case that fails does so for the reason
# it names and the SONAME assertion runs against a genuine ELF file rather than
# a fixture.  Case 0 is the control, and it doubles as the proof that `cp -a'
# preserves the chain -- which is what the drop-in job relies on when it stages
# the tree for the consumers.
# ---------------------------------------------------------------------------
self_test() {
    src=$1
    [ -d "$src" ] || die "self-test needs the staged directory; '$src' is not one"

    stage=$src/soname.stage
    [ -f "$stage" ] || die "self-test needs $stage; run \`make rust' first"
    sov=$(sed -n 1p "$stage")
    som=$(sed -n 2p "$stage")

    # The cases below are the good tree MINUS one thing each, so the good tree has
    # to be good first.  Checking that here rather than relying on the caller's
    # ordering means a broken staged tree is reported as a broken staged tree,
    # with the verifier's own diagnosis, instead of as a `cp' that could not find
    # a file.
    if ! ( verify "$src" '' ) > /dev/null 2>&1; then
        printf '%s: the tree the self-test would mutate does not itself verify:\n' \
            "$ME" >&2
        ( verify "$src" '' ) >&2 || true
        die "run \`$ME verify $src' and fix the staged chain first; the self-test \
mutates a KNOWN-GOOD tree and has nothing to say about a broken one."
    fi

    work=${TMPDIR:-/tmp}/dropin-chain-selftest.$$
    rm -rf "$work"
    mkdir -p "$work"
    # Removed on any exit, including a failure, so a self-test that fails leaves
    # no half-broken tree behind for the next run to trip over.
    trap 'rm -rf "$work"' EXIT HUP INT TERM

    passes=0
    failures=0

    # Archive-copy the four artifacts plus the descriptor.  `cp -a' implies
    # --no-dereference --preserve=links, which is the whole point: a link stays
    # a link.
    fresh() {
        rm -rf "$work/case"
        mkdir -p "$work/case"
        cp -a "$src/$sov" "$src/$som" "$src/libz.so" "$src/libz.a" \
            "$src/soname.stage" "$work/case/"
    }

    # `verify' reports a broken chain by calling `fail', which exits; run it in a
    # SUBSHELL so that the exit ends the case rather than the self-test, and so
    # that a case cannot leak a variable into the next one.
    expect() {
        want=$1
        name=$2
        if ( verify "$work/case" '' ) > "$work/out" 2>&1; then
            got=pass
        else
            got=fail
        fi
        if [ "$got" = "$want" ]; then
            passes=$((passes + 1))
            printf '  ok       %-46s (%s)\n' "$name" "$got"
        else
            failures=$((failures + 1))
            printf '  NOT OK   %-46s (wanted %s, got %s)\n' "$name" "$want" "$got"
            sed 's/^/           | /' "$work/out"
        fi
    }

    printf '%s: self-test of the chain verifier, against a copy of %s\n' "$ME" "$src"

    fresh
    expect pass 'an archive copy of the staged tree'

    fresh
    rm -f "$work/case/$som"
    expect fail 'the major alias removed'

    fresh
    rm -f "$work/case/libz.so"
    cp -L "$src/libz.so" "$work/case/libz.so"
    expect fail 'libz.so flattened by cp -L into a regular file'

    fresh
    rm -f "$work/case/$som"
    ln -s "$work/case/$sov" "$work/case/$som"
    expect fail 'the major alias retargeted to an absolute path'

    fresh
    rm -f "$work/case/$som"
    ln -s libz.so.99.99.99 "$work/case/$som"
    expect fail 'the major alias left dangling'

    fresh
    rm -f "$work/case/$sov"
    expect fail 'the versioned file removed, aliases kept'

    fresh
    rm -f "$work/case/$sov"
    ln -s /lib/x86_64-linux-gnu/libz.so.1 "$work/case/$sov"
    expect fail 'the versioned file replaced by a link to the system zlib'

    fresh
    rm -f "$work/case/libz.a"
    expect fail 'the static archive removed'

    fresh
    sed -n '1,2p' "$src/soname.stage" > "$work/case/soname.stage"
    expect fail 'the descriptor truncated to two lines'

    fresh
    { printf '%s\n' "$sov"; printf 'libz.so.99\n'; printf 'required\n'; } \
        > "$work/case/soname.stage"
    expect fail 'the descriptor naming an alias the SONAME contradicts'

    fresh
    { printf '../%s\n' "$sov"; printf '%s\n' "$som"; printf 'required\n'; } \
        > "$work/case/soname.stage"
    expect fail 'the descriptor naming a path rather than a file name'

    printf '%s: self-test %s -- %d passed, %d failed\n' \
        "$ME" "$([ "$failures" -eq 0 ] && echo PASS || echo FAIL)" \
        "$passes" "$failures"
    [ "$failures" -eq 0 ] || fail "the chain verifier did not behave as documented; \
$failures case(s) went the wrong way.  Fix the verifier -- a gate that cannot \
fail is not a gate."
}

# ---------------------------------------------------------------------------
# argument handling
# ---------------------------------------------------------------------------
[ $# -ge 1 ] || die "usage: $ME verify <staged-dir> [--expect-version <ver>]
       $ME self-test <staged-dir>"

command=$1
shift

case $command in
    verify)
        [ $# -ge 1 ] || die "verify needs the staged directory"
        target_dir=$1
        shift
        want_version=
        while [ $# -gt 0 ]; do
            case $1 in
                --expect-version)
                    [ $# -ge 2 ] || die "--expect-version needs a value"
                    want_version=$2
                    shift 2
                    ;;
                *) die "unknown argument '$1'" ;;
            esac
        done
        verify "$target_dir" "$want_version"
        ;;
    self-test)
        [ $# -ge 1 ] || die "self-test needs the staged directory"
        self_test "$1"
        ;;
    *) die "unknown command '$command'; expected verify or self-test" ;;
esac
