#!/bin/sh
# whitespace_gate.sh -- whitespace hygiene over the files the Rust port owns.
#
# WHY A GATE RATHER THAN A CONVENTION.  `git diff --check' refuses a diff that
# adds trailing whitespace or a blank line at end of file, and it is what a
# reviewer's tooling uses; a file that already carries either one therefore turns
# every later diff that touches it into a diff that "fails the check", and the
# usual response is to stop running the check.  One such file had accumulated
# here: .github/workflows/rust.yml ended `retention-days: 7\n\n', so
# `git diff --check' exited non-zero on it.  This script is how that stays fixed.
#
# WHAT IS CHECKED, on every scoped file:
#   * no trailing space or tab on any line
#   * no carriage return anywhere (LF endings only)
#   * the file ends with exactly one newline -- not zero, not two
#   * no literal tab in .rs, .toml, .yml, .md or .py, where indentation is spaces
#     by policy (rustfmt, the YAML spec, and the .py files here)
#
# WHAT IS NOT A FILE THIS GATE HAS ANYTHING TO SAY ABOUT.  Two kinds are skipped,
# and each is skipped for a reason rather than because it was inconvenient:
#
#   * CORPUS FIXTURES under crates/zlib-rs-differential/corpus/minimal/.  Those
#     are DATA -- the exact bytes the differential matrix compresses -- so a
#     trailing newline is not hygiene there, it is a different fixture.  Several
#     are deliberately raw: random.bin is incompressible noise that contains CR
#     bytes and bytes that look like trailing spaces, and empty.bin is zero bytes.
#   * BINARY files anywhere in scope, detected rather than listed (`grep -I'
#     reports no match in a binary file), so a fixture added elsewhere cannot make
#     this gate complain about the content of a byte array.
#
# WHAT IS IN SCOPE, AND WHY IT IS NOT THE WHOLE REPOSITORY.  Only files the port
# introduced.  The tree it was added to is upstream zlib, and upstream files carry
# whitespace this port has no business rewriting: Makefile.in has no newline at
# end of file and a `# ' line inside a commented-out block, both of them at the
# commit this port branched from.  "Repository-wide" would therefore mean either
# reformatting files the port does not own -- churn in a diff a reviewer has to
# read -- or an allowlist that grows quietly.  Scoping to the port's own files is
# the honest form, and the scope is enumerated below rather than inferred, so
# adding a file to the port without adding it here is visible.
#
# NON-VACUITY.  The script FAILS when the scoped list comes out empty, because a
# `git ls-files' pattern that stopped matching would otherwise check nothing and
# report success -- which is the failure mode of every list-driven gate.
#
# USAGE
#   whitespace_gate.sh            check, and print a one-line summary
#   whitespace_gate.sh --list     print the scoped files and exit

set -eu

ME=whitespace_gate.sh

# The port's own files.  `git ls-files' expands these against the index, so a
# pattern naming nothing contributes nothing and the emptiness check below
# catches the case where they all do.
SCOPE='
Cargo.toml
Cargo.lock
rust-toolchain.toml
.rustfmt.toml
clippy.toml
deny.toml
cbindgen.toml
crates
benches
fuzz
rust
.github/workflows/rust.yml
.github/scripts
'

# Fixture bytes are the fixture; see the note above.
EXCLUDE=':(exclude)crates/zlib-rs-differential/corpus/minimal'

# UNQUOTED ON PURPOSE, AND SUPPRESSED RATHER THAN LEFT AS NOISE.  $SCOPE is a
# newline-separated list and `git ls-files' wants each entry as its own operand,
# so the field splitting IS the mechanism here: quoting it would hand git ONE
# pathspec containing embedded newlines, which matches nothing, and the emptiness
# check below would then fail every run.  The split is safe as well as intended --
# SCOPE is an internal constant defined a few lines above, every entry is a
# literal path with no whitespace inside it and no glob metacharacter, so pathname
# expansion has nothing to expand and each line becomes exactly one operand.  The
# exclude pathspec is quoted because it is a single operand whose `:(exclude)'
# prefix must survive intact.  ShellCheck cannot see any of that, so it reports
# SC2086 here; the suppression is scoped to this one line so that the same
# diagnostic anywhere else in this file is still a real finding.
# shellcheck disable=SC2086
files=$(git ls-files -- $SCOPE "$EXCLUDE")

if [ "${1:-}" = '--list' ]; then
    printf '%s\n' "$files"
    exit 0
elif [ $# -ne 0 ]; then
    printf '%s: usage: %s [--list]\n' "$ME" "$ME" >&2
    exit 2
fi

if [ -z "$files" ]; then
    printf '::error::%s: the scoped file list is EMPTY, so nothing was checked.  \
The patterns in SCOPE no longer match anything in the index.\n' "$ME" >&2
    exit 1
fi

count=$(printf '%s\n' "$files" | wc -l | tr -d ' ')
failures=0

report() {
    printf '::error::%s: %s: %s\n' "$ME" "$1" "$2" >&2
    failures=$((failures + 1))
}

for file in $files; do
    # A deleted-but-still-indexed path, or a submodule gitlink, is not a file to
    # read.  Skipping it is correct rather than lenient: there is no content.
    [ -f "$file" ] || continue

    # Binary, or empty.  `grep -I' treats a binary file as non-matching, so this
    # is one test for both, and an empty file has no whitespace to be wrong about.
    grep -Iq . "$file" 2> /dev/null || continue

    if grep -n '[ 	]$' "$file" > /dev/null 2>&1; then
        lines=$(grep -c '[ 	]$' "$file")
        report "$file" "$lines line(s) end in a space or a tab; \
\`git diff --check' refuses those"
        grep -n '[ 	]$' "$file" | head -5 | sed 's/^/           /' >&2
    fi

    if grep -q "$(printf '\r')" "$file" 2>/dev/null; then
        report "$file" "contains a carriage return; this tree is LF-only"
    fi

    # `tail -c 2' plus `od' rather than reading the whole file: the question is
    # about the last two bytes and nothing else.
    last2=$(tail -c 2 "$file" | od -An -c | tr -d ' \n')
    case $last2 in
        '\n\n')
            report "$file" "ends with a BLANK LINE; \`git diff --check' refuses \
that, and every later diff touching this file inherits the complaint"
            ;;
        *'\n') ;;
        *)
            report "$file" "does not end with a newline"
            ;;
    esac

    case $file in
        *.rs | *.toml | *.yml | *.yaml | *.md | *.py)
            if grep -n '	' "$file" > /dev/null 2>&1; then
                report "$file" "contains a literal tab; indentation in this file \
type is spaces (rustfmt, the YAML spec, and this tree's Python)"
                grep -n '	' "$file" | head -3 | sed 's/^/           /' >&2
            fi
            ;;
    esac
done

if [ "$failures" -ne 0 ]; then
    printf '%s: FAIL -- %d problem(s) across %d scoped file(s)\n' \
        "$ME" "$failures" "$count" >&2
    exit 1
fi

printf '%s: PASS -- %d scoped files: no trailing whitespace, no CR, exactly one trailing newline each\n' \
    "$ME" "$count"
