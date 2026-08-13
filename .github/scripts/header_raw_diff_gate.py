#!/usr/bin/env python3
"""Account for every line of AAP 0.8.6's `diff -u zlib.h generated_zlib.h`, and fail on any line no rule explains.

# What this closes

AAP 0.8.6 freezes a command:

    cbindgen --config cbindgen.toml --crate libz-rs-sys --output generated_zlib.h
    diff -u zlib.h generated_zlib.h

and AAP 0.4.2.3 says divergence in any public signature fails the build. That command's
output cannot be empty, and `cbindgen.toml` says why in eleven enumerated rules -- D1
decoration, D3 includes, D5 large-file duplicates, D7 ordering, D8 comment text, D9 `#endif`
labels, D10 `extern "C"` scaffolding, and so on. Measured on this tree it is **roughly six thousand lines in
a single hunk**: the two files share so few consecutive lines that `diff` finds no alignment
at all, so essentially every line of each file appears.

The workflow used to run that command, write the output to an artifact, and describe it as
"necessarily non-empty ... the artifact is how a reviewer confirms that what it contains is
only those reasons." Nothing confirmed it. A reviewer confirming six thousand lines by eye, once,
is not a gate, and the claim "only those reasons" was the one thing never checked.

# What this asserts, and what it does not

It classifies **every line** of the raw diff against a rule, and the residue must be zero.
Each rule names the mechanism that checks its class exhaustively, because classification on
its own would be worthless -- a changed signature is still "a declaration line".

| Class | Rule | What actually checks this class |
|---|---|---|
| blank, comment body, `/*`…`*/` | D8 | nothing needs to: comment text is not the contract |
| `#include` | D3 | the generated header is compiled standalone as C89/C99/C17/C++17 |
| `#if`/`#ifdef`/`#else`/`#endif` | D3, D6, D9 | the same standalone compiles, under both `_LARGEFILE64_SOURCE` settings |
| `extern "C" {`, closing `}` | D10 | the same standalone compiles, including as C++17 |
| `#define`, `#undef` | D4 | rule 10 of the header gate compares integer object-like macros BY VALUE |
| `typedef` | D3 | `crates/libz-rs-sys/src/layout_assertions.rs` asserts every size and offset at compile time |
| struct/union member | D2 | the same layout assertions, plus `tests/abi_layout.rs` at run time |
| declaration of a contract function | D1, D5, D7 | pass A compares the ZEXTERN name sets both ways; pass D diffs the RESOLVED signatures and must be empty |
| anything else | — | **nothing. This is the residue, and it fails.** |

So the guarantee is precise: the raw diff contains nothing except lines belonging to
categories that a named, exhaustive gate already covers. A new *kind* of divergence -- a
declaration of a function that is not in the contract, a stray statement, an unbalanced
construct -- lands in the residue and fails here.

It also re-derives the two function-name sets itself and compares them, so this script is not
merely a classifier deferring to a gate it cannot see: the deferral that matters most is
checked twice, once here and once by pass A.

# Usage

    python3 .github/scripts/header_raw_diff_gate.py \\
        --reference zlib.h \\
        --generated target/rust-header/generated_zlib.h \\
        [--allow-missing gzopen_w] [--allow-extra inflate_table]

The diff is computed here rather than read from a file, so the command that is gated is the
one AAP 0.8.6 names.
"""

from __future__ import annotations

import argparse
import re
import subprocess
import sys
from collections import Counter
from dataclasses import dataclass, field
from pathlib import Path

#: Every rule this gate recognises: identifier -> (what the class is, what checks it).
RULES: dict[str, tuple[str, str]] = {
    "D8": (
        "blank lines and comment text",
        "nothing needs to -- comment text is not part of the ABI contract",
    ),
    "D3": (
        "#include lines, conditional-compilation directives and typedefs",
        "the generated header is compiled standalone as C89/C99/C17/C++17 with warnings "
        "fatal, under both _LARGEFILE64_SOURCE settings",
    ),
    "D4": (
        "#define and #undef",
        "rule 10 of the header job compares every integer object-like macro BY VALUE",
    ),
    "D2": (
        "struct and union members",
        "crates/libz-rs-sys/src/layout_assertions.rs asserts every size and field offset at "
        "compile time; tests/abi_layout.rs repeats it at run time",
    ),
    "D10": (
        'extern "C" linkage scaffolding',
        "the standalone C++17 compile of the generated header",
    ),
    "DECL": (
        "declarations of contract functions",
        "pass A compares the ZEXTERN name sets in both directions and pass D diffs the "
        "resolved signatures, which must be empty; this script re-derives and compares the "
        "name sets independently",
    ),
}

#: `ZEXTERN`-style declaration decoration, deleted before a line is examined (D1).
DECORATION = re.compile(r"\b(?:ZEXTERN|ZEXPORTVA|ZEXPORT|FAR|z_const|ZLIB_INTERNAL)\b")

#: A preprocessor directive, with its keyword captured.
DIRECTIVE = re.compile(r"^\s*#\s*(\w+)")

#: The directives D3, D6 and D9 account for.
CONDITIONALS = frozenset(
    {"if", "ifdef", "ifndef", "else", "elif", "endif", "include", "include_next", "pragma", "error"}
)

#: The directives D4 accounts for.
MACRO_DIRECTIVES = frozenset({"define", "undef"})

#: A function declaration's name: the last identifier before the parameter list.
DECL_NAME = re.compile(r"(\w+)\s*\(")

#: One whole `ZEXTERN ... ;` declaration in zlib.h.
#:
#: Matched as a STATEMENT rather than used to filter statements produced by splitting on `;`,
#: and that distinction cost a debugging round: splitting first leaves the tail of whatever
#: preceded each declaration attached to it, so an anchored `^\s*ZEXTERN` filter silently
#: dropped twelve real declarations -- `zlibVersion`, `compress`, `adler32`, `zError` and the
#: large-file spellings among them -- and reported 86 where there are 96.
ZEXTERN_STATEMENT = re.compile(r"\bZEXTERN\b[^;]*;", re.S)


@dataclass
class SideState:
    """Line-classification state for one side of the diff.

    Both `-` and `+` lines are classified, and each side needs its own state because the two
    files are interleaved in the diff and a `/* ... */` opened on one side says nothing about
    the other.
    """

    #: Inside a `/* ... */` that has not closed yet.
    in_comment: bool = False
    #: Inside a `{ ... }` struct or union body.
    brace_depth: int = 0
    #: Inside a construct whose parentheses have not closed yet.
    open_parens: int = 0
    #: The previous line ended with a backslash, so this one continues a `#define`.
    in_macro: bool = False
    #: The rule the currently-open multi-line construct was classified under, so its
    #: continuation lines are attributed to the same rule rather than falling into the
    #: residue. Measured: without this, the second line of `typedef unsigned (*in_func)(...`
    #: and the bodies of the six multi-line `#define`s in zlib.h were unexplained.
    continuation_rule: str | None = None
    #: Rule -> line count.
    tally: Counter[str] = field(default_factory=Counter)
    #: Names of contract-shaped declarations seen.
    names: set[str] = field(default_factory=set)


def strip_comments(line: str, state: SideState) -> str:
    """Remove comment text from `line`, maintaining `state.in_comment` across lines."""
    out = []
    index = 0
    while index < len(line):
        if state.in_comment:
            end = line.find("*/", index)
            if end < 0:
                return "".join(out)
            state.in_comment = False
            index = end + 2
            continue
        start = line.find("/*", index)
        line_comment = line.find("//", index)
        if line_comment >= 0 and (start < 0 or line_comment < start):
            out.append(line[index:line_comment])
            return "".join(out)
        if start < 0:
            out.append(line[index:])
            return "".join(out)
        out.append(line[index:start])
        state.in_comment = True
        index = start + 2
    return "".join(out)


#: `struct internal_state;` and friends: a forward declaration of an opaque tag.
FORWARD_TAG = re.compile(r"^(?:struct|union|enum)\s+\w+\s*;$")


def classify(raw_line: str, state: SideState) -> str | None:
    """The rule accounting for `raw_line`, or `None` when nothing does.

    Order matters twice over. Comment state is consumed first, because a line inside a
    `/* */` may look like anything at all; and CONTINUATION state is consumed next, because
    the second line of a `#define` or of a wrapped parameter list is not classifiable on its
    own -- it is part of a construct whose first line already was.
    """
    was_in_comment = state.in_comment
    code = strip_comments(raw_line, state)
    stripped = code.strip()

    # A `#define` body continues while the previous line ended with a backslash. zlib.h has
    # six such macros -- deflateInit, deflateInit2, inflateInit, inflateInit2, gzgetc and
    # their large-file spellings -- and their bodies are ordinary C that no other rule fits.
    if state.in_macro:
        state.in_macro = stripped.endswith("\\")
        return "D4"

    if not stripped:
        # Blank, whitespace, or a line that was nothing but comment.
        return "D8" if (was_in_comment or code != raw_line or not raw_line.strip()) else None

    directive = DIRECTIVE.match(stripped)
    if directive is not None:
        keyword = directive.group(1)
        state.in_macro = stripped.endswith("\\")
        if keyword in MACRO_DIRECTIVES:
            return "D4"
        if keyword in CONDITIONALS:
            return "D3"
        return None

    body = DECORATION.sub(" ", stripped).strip()
    if not body:
        # A line that was nothing but declaration decoration -- zlib.h writes `ZEXTERN` on a
        # line of its own in places.
        return "DECL"

    # A construct opened on an earlier line. Its rule was decided then.
    if state.open_parens > 0:
        state.open_parens = max(0, state.open_parens + body.count("(") - body.count(")"))
        return state.continuation_rule or "DECL"

    if body.startswith('extern "C"') or (body == "}" and state.brace_depth == 0):
        return "D10"

    # Struct and union bodies. Tracked by brace depth so member lines -- which look like
    # ordinary declarations -- are attributed to D2 and not to DECL. A bare
    # `struct internal_state;` is the opaque-state forward declaration and belongs here too:
    # what it stands for is checked by the layout assertions, not by its spelling.
    opens = body.count("{")
    closes = body.count("}")
    inside = state.brace_depth > 0
    state.brace_depth = max(0, state.brace_depth + opens - closes)
    if inside or FORWARD_TAG.match(body) or (opens and re.search(r"\b(?:struct|union|enum)\b", body)):
        return "D2"

    state.open_parens = max(0, body.count("(") - body.count(")"))
    if body.startswith("typedef"):
        state.continuation_rule = "D3"
        return "D3"
    state.continuation_rule = "DECL"

    match = DECL_NAME.search(body)
    if match is not None and (body.endswith(";") or state.open_parens > 0 or body.endswith(",")):
        name = match.group(1)
        # `OF((...))` wrappers and macro-like forms put the real name earlier; take the
        # first identifier that is followed by `(` and is not a keyword.
        for candidate in DECL_NAME.finditer(body):
            token = candidate.group(1)
            if token not in {"OF", "if", "while", "for", "switch", "sizeof", "return"}:
                name = token
                break
        state.names.add(name)
        return "DECL"

    state.continuation_rule = None
    return None


def declared_names(path: Path, statement: re.Pattern[str] | None) -> set[str]:
    """Function names declared by `path`.

    `statement` selects whole declarations when the file marks them -- `zlib.h` does, with
    `ZEXTERN`. When it is `None` every `;`-terminated statement is considered, which is right
    for the generated header: cbindgen emits one plain declaration per statement inside a
    single `extern "C"` block and nothing else that ends in a semicolon.
    """
    text = path.read_text(encoding="utf-8", errors="replace")
    text = re.sub(r"/\*.*?\*/", " ", text, flags=re.S)
    text = re.sub(r"//[^\n]*", " ", text)
    # Preprocessor lines go before the statements are cut, and this too cost a round: a
    # `;`-delimited piece of the generated header begins with whatever directives preceded the
    # declaration, so `#if defined(ZLIB_RS_LIBZ_COMPAT) int deflate(...)` made the first
    # identifier-followed-by-`(` be `defined`, and the scan reported five names instead of
    # ninety-five. Continuation lines go with their directive.
    lines, skipping = [], False
    for line in text.splitlines():
        stripped = line.strip()
        if skipping or stripped.startswith("#"):
            skipping = stripped.endswith("\\")
            continue
        lines.append(line)
    text = "\n".join(lines)
    # The D10 linkage scaffolding, removed for the same reason: `extern "C" {` sits in the
    # same `;`-delimited piece as the first declaration after it, and the `{` filter below
    # would otherwise drop that one declaration and only that one. Measured: `adler32`, the
    # first name cbindgen emits, was the single missing name until this was added.
    text = re.sub(r'extern\s*"C"\s*\{', " ", text)
    text = "\n".join(line for line in text.splitlines() if line.strip() != "}")
    pieces = (
        (found.group(0) for found in statement.finditer(text))
        if statement is not None
        else iter(text.split(";"))
    )
    names: set[str] = set()
    for piece in pieces:
        flat = " ".join(piece.split())
        if not flat or "(" not in flat or "{" in flat:
            continue
        if flat.lstrip().startswith("typedef"):
            continue
        candidate = DECORATION.sub(" ", flat)
        # `ZEXTERN int ZEXPORT deflate OF((z_streamp, int));` flattens with `OF` first, so the
        # first identifier followed by `(` that is not the wrapper is the name.
        token = None
        for found in DECL_NAME.finditer(candidate):
            if found.group(1) != "OF":
                token = found.group(1)
                break
        if token is not None:
            names.add(token)
    return names


def main() -> int:
    """Run the AAP 0.8.6 diff, classify every line, and fail on any residue."""
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--reference", default="zlib.h", help="the immutable contract header")
    parser.add_argument(
        "--generated",
        default="target/rust-header/generated_zlib.h",
        help="the cbindgen-generated header",
    )
    parser.add_argument(
        "--allow-missing",
        action="append",
        default=[],
        help="a contract name the generated header may omit (D11, and the _WIN32-only names)",
    )
    parser.add_argument(
        "--allow-extra",
        action="append",
        default=[],
        help="a name the generated header may declare that the contract does not",
    )
    parser.add_argument("--write-diff", default=None, help="also write the raw diff here")
    parser.add_argument(
        "--signature-evidence",
        default=None,
        metavar="PATH",
        help=(
            "pass D's signatures.diff. Required whenever the DECL rule matches, because the "
            "DECL rule accounts for a declaration line WITHOUT reading the types in it: a "
            "changed parameter type is still a declaration line. This file existing and "
            "being empty is what makes that deferral true rather than assumed."
        ),
    )
    parser.add_argument(
        "--accounting-only",
        action="store_true",
        help=(
            "run the line accounting WITHOUT pass D's evidence. Prints an UNVERIFIED banner "
            "and does NOT constitute a header-parity pass; for local exploration only, and "
            "never used by CI."
        ),
    )
    arguments = parser.parse_args()

    reference = Path(arguments.reference)
    generated = Path(arguments.generated)
    for path in (reference, generated):
        if not path.is_file():
            sys.exit(f"error: {path} does not exist")

    # AAP 0.8.6's command, verbatim.
    completed = subprocess.run(  # noqa: S603 -- fixed argv, no shell
        ["diff", "-u", str(reference), str(generated)],
        capture_output=True,
        text=True,
        check=False,
        stdin=subprocess.DEVNULL,
    )
    if completed.returncode > 1:
        sys.exit(
            f"error: `diff -u {reference} {generated}` failed to run: "
            f"{completed.stderr.strip()[:2000]}"
        )
    diff = completed.stdout.splitlines()
    if arguments.write_diff:
        Path(arguments.write_diff).write_text(completed.stdout, encoding="utf-8")

    print(f"AAP 0.8.6 `diff -u {reference} {generated}`: {len(diff)} line(s)")
    if not diff:
        # An empty diff would mean the generated header is byte-identical to the contract,
        # which cbindgen cannot produce; treat it as a broken invocation rather than success.
        sys.exit(
            "error: the diff is empty. cbindgen cannot reproduce zlib.h byte for byte -- D1, "
            "D3, D5, D7, D8, D9 and D10 each guarantee output no generator would match -- so "
            "an empty diff means the two paths point at the same file, or the generated "
            "header was never written."
        )

    minus, plus = SideState(), SideState()
    residue: list[tuple[int, str, str]] = []
    counted = 0
    for number, line in enumerate(diff, start=1):
        if line.startswith(("--- ", "+++ ", "@@ ", "\\ ")):
            continue
        if line.startswith("-"):
            side, payload, label = minus, line[1:], "-"
        elif line.startswith("+"):
            side, payload, label = plus, line[1:], "+"
        else:
            # A context line appears on both sides; feed it to both so the comment and brace
            # state of each stays truthful, and count it once.
            payload = line[1:] if line.startswith(" ") else line
            for other in (minus, plus):
                classify(payload, other)
            continue
        counted += 1
        rule = classify(payload, side)
        if rule is None:
            residue.append((number, label, payload))
        else:
            side.tally[rule] += 1

    print(f"classified {counted} changed line(s) against {len(RULES)} rule(s):")
    total = Counter()
    for name in RULES:
        removed, added = minus.tally[name], plus.tally[name]
        total[name] = removed + added
        if removed or added:
            what, checked_by = RULES[name]
            print(f"  {name:5} {removed + added:5}  ({removed} removed, {added} added)  {what}")
            print(f"        checked by: {checked_by}")

    if residue:
        print(f"\nerror: {len(residue)} line(s) of the AAP 0.8.6 diff match no rule.")
        print(
            "       Every line of that diff has to belong to a category some other gate\n"
            "       covers exhaustively; a line that belongs to none is an unexplained\n"
            "       divergence between the immutable contract in zlib.h and what the facade\n"
            "       generates, which AAP 0.4.2.3 makes a build failure. Either the change is\n"
            "       wrong, or it is a new KIND of accepted divergence -- in which case it\n"
            "       needs a rule in cbindgen.toml, a named mechanism that checks it, and an\n"
            "       entry in RULES above. First twenty:"
        )
        for number, label, payload in residue[:20]:
            print(f"       {number:6} {label} {payload}")
        return 1

    # The deferral that matters most, checked here as well as by pass A: the two name sets.
    contract = declared_names(reference, ZEXTERN_STATEMENT)
    if len(contract) < 90:
        sys.exit(
            f"error: only {len(contract)} ZEXTERN declarations found in {reference}; the scan "
            f"is wrong, and a scan that finds nothing would make this comparison vacuous"
        )
    produced = declared_names(generated, None)
    allow_missing = set(arguments.allow_missing)
    allow_extra = set(arguments.allow_extra)
    missing = sorted(contract - produced - allow_missing)
    extra = sorted(produced - contract - allow_extra)
    if missing:
        print(
            f"\nerror: the generated header does not declare {len(missing)} contract "
            f"function(s): {', '.join(missing)}.\n"
            f"       zlib.h is the immutable contract, so a name it declares and the facade "
            f"does not is a symbol a linked consumer will not find."
        )
        return 1
    if extra:
        # BOTH DIRECTIONS, and this one is not symmetry for its own sake: an added
        # declaration is how a header grows a surface the contract never promised, and it is
        # also the shape a stray or mis-cfg'd export takes. Anything legitimate is named on
        # the command line, so the allowance is visible in the workflow rather than implied.
        print(
            f"\nerror: the generated header declares {len(extra)} name(s) the contract does "
            f"not: {', '.join(extra)}.\n"
            f"       AAP 0.8.1 fixes the exported surface at the existing declarations -- no "
            f"additions. If one of these is legitimate, pass it as --allow-extra and say why "
            f"in cbindgen.toml."
        )
        return 1
    print(
        f"name sets: {len(contract)} ZEXTERN declaration(s) in {reference}, all of them "
        f"declared by the generated header"
        + (f" ({len(allow_missing)} allowed omission(s)" if allow_missing else "")
        + (f", {len(allow_extra)} allowed addition(s))" if allow_missing else "")
    )

    # The DECL deferral, made verifiable.  Everything above proves each line of the diff
    # is a KIND of line some other mechanism covers.  For DECL that mechanism is pass D,
    # and this gate cannot substitute for it: `int deflate(z_streamp, int)` and
    # `int deflate(z_streamp, long)` are both "a declaration line", so a changed parameter
    # type reaches this point classified and unremarked.  Requiring pass D's product here
    # is what stops that from being an assumption about a gate that might not have run.
    if total["DECL"]:
        if arguments.accounting_only:
            print(
                "\nUNVERIFIED: --accounting-only was passed, so the "
                f"{total['DECL']} DECL line(s) were counted but their TYPES were never "
                "compared. This run is line accounting only and is NOT a header-parity "
                "pass; pass D (the resolved-signature diff) is the mechanism for that class."
            )
        elif arguments.signature_evidence is None:
            print(
                f"\nerror: {total['DECL']} line(s) were classified DECL, which accounts for "
                f"a declaration WITHOUT comparing the types inside it -- a changed parameter "
                f"type is still a declaration line. Pass --signature-evidence PATH naming "
                f"pass D's signatures.diff so the deferral is checked, or --accounting-only "
                f"to state plainly that this run does not check it."
            )
            return 1
        else:
            evidence = Path(arguments.signature_evidence)
            if not evidence.is_file():
                print(
                    f"\nerror: {evidence} does not exist, so pass D's resolved-signature "
                    f"diff either never ran or never wrote its product. The {total['DECL']} "
                    f"DECL line(s) above are accounted for as declarations on the strength "
                    f"of that comparison, and this gate does not repeat it."
                )
                return 1
            body = evidence.read_text(encoding="utf-8", errors="replace")
            if body.strip():
                print(
                    f"\nerror: {evidence} is NOT empty, so the resolved signatures of the "
                    f"contract and the generated header differ. That diff is authoritative "
                    f"and the left side is the frozen contract:"
                )
                print(body[:8000])
                return 1
            print(
                f"deferral checked: pass D's {evidence} exists and is empty, so the "
                f"{total['DECL']} DECL line(s) are declarations of the same resolved "
                f"signatures rather than merely lines of the same shape"
            )

    print(
        f"\nPASS: every one of the {counted} changed line(s) in AAP 0.8.6's diff belongs to a "
        f"category a named gate covers, and the residue is empty."
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
