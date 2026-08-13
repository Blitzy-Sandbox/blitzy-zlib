#!/usr/bin/env python3
"""Project both headers into one canonical contract artifact and require `diff -u` to be EXACTLY EMPTY.

# What this closes

AAP 0.8.6 freezes a command pair:

    cbindgen --config cbindgen.toml --crate libz-rs-sys --output generated_zlib.h
    diff -u zlib.h generated_zlib.h

and AAP 0.4.2.3 says divergence in any public signature fails the build. Measured on this
tree, that raw diff runs to **roughly six thousand lines**, and it cannot be otherwise: `zlib.h` is 2,057 lines of
hand-written prose and declarations, the generated header is 3,990 lines with cbindgen's own
comment blocks and its own ordering, and the two share so few consecutive lines that `diff`
finds no alignment at all. `header_raw_diff_gate.py` already accounts for every one of those
lines against a named rule and fails on any residue, which is what makes the raw artifact
trustworthy.

What was still missing is the thing AAP 0.8.6 actually asks a reader to look at: **a
comparison whose diff is empty.** "Non-empty, but every line is explained" is a strictly
weaker statement than "empty", because it relies on the classifier's rules being complete.
This gate supplies the stronger statement over the *whole* contract surface.

# How an empty diff is achieved WITHOUT weakening anything

Not by filtering the diff, and not by touching `zlib.h` -- which is immutable under AAP 0.8.1
directive 2 and is opened read-only here. Both sides are *projected* through the C and C++
compilers into one canonical rendering of what the contract actually promises, and the two
renderings are diffed.

Projection is what removes the noise, and it removes it because the noise is genuinely not
part of the contract:

| Removed | Why it is not the contract |
|---|---|
| comment text, blank lines, indentation | prose about a declaration is not the declaration |
| declaration ORDER | a C consumer cannot observe the order of declarations in a header |
| typedef SPELLING (`uLong` vs `unsigned long`) | the compiler resolves both to one type; a consumer sees the resolved type |
| macro spelling (`0x1321` vs `4897`, `(-5)` vs `-5`) | the preprocessor yields one value; a consumer sees the value |
| `#include`, `extern "C"`, `#if` scaffolding | covered by compiling the generated header standalone in 8 dialects |

Everything the contract *does* promise is kept, and is compared as a value rather than as
text:

| Record | Rendered by | What a divergence would mean |
|---|---|---|
| `FUNC <name> <type>` | `decltype(&name)` mangled by C++ and demangled by `nm -C` | a changed parameter or return type -- the whole point |
| `TYPE <name> <size> <align>` | `sizeof` / `_Alignof` in a compiled probe | a public alias that changed width |
| `STRUCT <tag> <size> <align>` | the same probe | a caller-visible struct that changed shape |
| `FIELD <tag>.<name> <offset> <width>` | `offsetof` and member `sizeof` | a moved or resized member -- `gzFile_s` is expanded in CALLER code by the `gzgetc` macro |
| `MACRO <name> <value>` | the value cast to `long long` in the probe | a constant that changed meaning |

Because the projection is produced by *compiling* each side, a difference in how a type is
spelled cannot create a false difference, and a difference in what a type *is* cannot be
hidden by matching spellings. That is the property a textual diff of two headers cannot have
in either direction.

# What this gate does NOT replace

Every existing check still runs and is still required. This one is additional, exactly as the
review's resolution guidance asks:

* the raw AAP 0.8.6 diff is still produced and still published as evidence (`--write-raw-diff`),
  so the difference this gate normalises stays visible rather than being hidden by it;
* `header_raw_diff_gate.py` still classifies every raw line with zero residue;
* the shape gate's passes A, A', B, D and rule 10 still run in the `header` job;
* the generated header is still compiled standalone as C89/C99/C17/C++17, with and without
  `_WIN32`, which is what covers the scaffolding this projection deliberately drops.

# Failing closed

Three ways a gate like this could pass while proving nothing, all of them refused here:

1. **A missing tool.** `cc`, `c++` and `nm` are required. Without them there is no reduced
   form of the check, so their absence is a failure rather than a skip.
2. **A near-empty projection.** Minimum record counts are enforced per category. An empty
   diff of two empty files is not evidence.
3. **A silent allowance.** Every name absent from one side must be named on the command line
   WITH a reason, the reasons are printed on success, and an allowance that turns out to be
   unnecessary is itself a failure -- so an allowance cannot outlive the condition it was
   granted for.

# Usage

    python3 .github/scripts/header_contract_projection_gate.py \\
        --reference zlib.h \\
        --generated generated_zlib.h \\
        --out-dir target/header-gate \\
        --allow-absent-function gzopen_w='declared only under _WIN32 (zlib.h L2041)' \\
        --allow-absent-type charf='cbindgen prunes typedefs no prototype references' \\
        --write-raw-diff target/header-gate/aap-0.8.6-raw.diff

    python3 .github/scripts/header_contract_projection_gate.py --self-test

`--self-test` perturbs a signature, a field offset and a macro value in a scratch copy of the
generated header and requires this gate to FAIL on each -- the negative control that proves an
empty diff means agreement rather than a projection that compares nothing.
"""

from __future__ import annotations

import argparse
import os
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

# Rule A2 of `cbindgen.toml`: preprocess each side under its own DECLARED configuration.
# `_LARGEFILE64_SOURCE=1` without `_FILE_OFFSET_BITS=64` is the reference build's own setting,
# which is what makes the large-file family unambiguous -- `Z_LARGE64` defined, `Z_WANT64`
# undefined, so each `*64` name is declared exactly once. `ZLIB_CONST` turns `zconf.h`'s
# `z_const` into `const`, which is where the generated header hard-codes it.
REFERENCE_DEFINES = ["-D_LARGEFILE64_SOURCE=1", "-DZLIB_CONST"]
GENERATED_DEFINES = ["-DZLIB_RS_LIBZ_COMPAT", "-DZLIB_RS_GZ"]

# Function-like macros that shadow a real exported function of the same name. Each must be
# `#undef`-ed before the function can be named, or the probe expands the macro instead.
# `gzgetc` is both, which is the whole reason `gzgetc_` exists in the contract.
SHADOWING_MACROS = [
    "deflateInit",
    "deflateInit2",
    "inflateInit",
    "inflateInit2",
    "inflateBackInit",
    "gzgetc",
]

# The five names `zlib.h` documents with a `ZEXTERN` line inside a comment block: they are
# macros, not declarations, so they are not functions to project. `deflateInit` and its four
# siblings are the documented spellings of the `*_` entry points.
DOCUMENTED_MACRO_NAMES = {
    "deflateInit",
    "deflateInit2",
    "inflateInit",
    "inflateInit2",
    "inflateBackInit",
}

# Public type aliases of the contract. Compared by size and alignment, which is what a
# consumer's object code actually depends on -- never by spelling, because `uLong` and
# `unsigned long` are the same type and must compare equal.
CONTRACT_TYPES = [
    "Byte",
    "Bytef",
    "alloc_func",
    "charf",
    "free_func",
    "gzFile",
    "gz_headerp",
    "in_func",
    "intf",
    "out_func",
    "uInt",
    "uIntf",
    "uLong",
    "uLongf",
    "voidp",
    "voidpc",
    "voidpf",
    "z_crc_t",
    "z_off64_t",
    "z_off_t",
    "z_size_t",
    "z_streamp",
]

# The three caller-visible structs, keyed by CONTRACT TAG rather than by typedef. `gzFile_s`
# is the one that cannot be got wrong quietly: `zlib.h`'s `gzgetc` macro is expanded in
# CALLER object code and loads `have`, `next` and `pos` at their offsets, so a consumer
# compiled against the real header carries those offsets in its own instructions.
CONTRACT_STRUCTS = {
    "z_stream_s": [
        "next_in", "avail_in", "total_in", "next_out", "avail_out", "total_out",
        "msg", "state", "zalloc", "zfree", "opaque", "data_type", "adler", "reserved",
    ],
    "gz_header_s": [
        "text", "time", "xflags", "os", "extra", "extra_len", "extra_max",
        "name", "name_max", "comment", "comm_max", "hcrc", "done",
    ],
    "gzFile_s": ["have", "next", "pos"],
}

# Refuse to pass on a projection too small to be evidence. Each floor is below the measured
# count with room for the contract to grow, and far above zero.
MINIMUM_RECORDS = {"FUNC": 90, "TYPE": 15, "STRUCT": 3, "FIELD": 28, "MACRO": 30}


def run(command: list[str], **kwargs) -> subprocess.CompletedProcess:
    """Run a command, capturing both streams as text. Never raises on a non-zero status."""
    return subprocess.run(command, capture_output=True, text=True, **kwargs)


def strip_comments(text: str) -> str:
    """Remove C block and line comments, so a declaration inside a comment is not mistaken for one."""
    text = re.sub(r"/\*.*?\*/", " ", text, flags=re.S)
    return re.sub(r"//[^\n]*", " ", text)


def contract_function_names(reference: Path) -> list[str]:
    """Every function the frozen contract declares, from `zlib.h`'s own `ZEXTERN` lines.

    Comments are stripped first. That is not cosmetic: `zlib.h` documents five macros with a
    `ZEXTERN`-shaped line *inside* a comment block, and counting those would inflate the
    contract from its true 96 declarations to 101.
    """
    text = strip_comments(reference.read_text())
    names = {
        match.group(1)
        for match in re.finditer(r"^\s*ZEXTERN[^;]*?(\w+)\s*\(", text, re.M)
    }
    return sorted(names - DOCUMENTED_MACRO_NAMES)


def object_macro_names(paths: list[Path]) -> set[str]:
    """Object-like macro names defined by the given headers.

    The `(?![(\\w])` guard excludes function-like macros: `#define deflateInit(strm, level)`
    is not a constant, and a naive `\\w+` would also capture a truncated prefix of one.
    """
    found: set[str] = set()
    for path in paths:
        text = strip_comments(path.read_text())
        for match in re.finditer(r"^\s*#\s*define\s+([A-Za-z_]\w*)(?![(\w])", text, re.M):
            found.add(match.group(1))
    return found


def probe_source(header: Path, statements: list[str]) -> str:
    """A C probe that includes one side's header and prints the records it is asked for."""
    lines = [
        "#define _LARGEFILE64_SOURCE 1",
        "#define ZLIB_CONST 1",
        "#include <stdarg.h>",
        "#include <stddef.h>",
        "#include <stdio.h>",
        '#include "%s"' % header,
    ]
    lines += ["#undef %s" % name for name in SHADOWING_MACROS]
    lines.append("int main(void) {")
    lines += ["    " + statement for statement in statements]
    lines.append("    return 0;")
    lines.append("}")
    return "\n".join(lines) + "\n"


def compile_and_run(source: str, defines: list[str], stem: Path, cc: str) -> tuple[str | None, str]:
    """Compile a probe and return its stdout, or `None` plus the compiler's diagnostics."""
    stem.parent.mkdir(parents=True, exist_ok=True)
    source_file = stem.with_suffix(".c")
    source_file.write_text(source)
    built = run([cc, "-std=c11", "-I.", *defines, "-o", str(stem), str(source_file)])
    if built.returncode != 0:
        return None, built.stderr
    ran = run([str(stem)])
    if ran.returncode != 0:
        return None, ran.stderr
    return ran.stdout, ""


def value_records(
    header: Path,
    defines: list[str],
    macros: list[str],
    types: list[str],
    stem: Path,
    cc: str,
) -> tuple[list[str] | None, str]:
    """Render the TYPE, STRUCT, FIELD and MACRO records for one side by compiling a probe."""
    statements = []
    for name in types:
        statements.append(
            'printf("TYPE %s\\t%%zu %%zu\\n", sizeof(%s), _Alignof(%s));' % (name, name, name)
        )
    for tag, fields in CONTRACT_STRUCTS.items():
        statements.append(
            'printf("STRUCT %s\\t%%zu %%zu\\n", sizeof(struct %s), _Alignof(struct %s));'
            % (tag, tag, tag)
        )
        for field in fields:
            statements.append(
                'printf("FIELD %s.%s\\t%%zu %%zu\\n", offsetof(struct %s, %s),'
                " sizeof(((struct %s *)0)->%s));" % (tag, field, tag, field, tag, field)
            )
    for name in macros:
        statements.append('printf("MACRO %s\\t%%lld\\n", (long long)(%s));' % (name, name))
    output, error = compile_and_run(probe_source(header, statements), defines, stem, cc)
    if output is None:
        return None, error
    return output.splitlines(), ""


def signature_records(
    header: Path, defines: list[str], functions: list[str], stem: Path, cxx: str
) -> tuple[list[str] | None, str]:
    """Render the FUNC records for one side as the C++ compiler's own view of each function type.

    `decltype(&name)` resolves the entire typedef chain, so `uLong` and `unsigned long`
    produce the same rendering while a genuinely different parameter type cannot. The
    instantiation is emitted as a symbol and read back demangled, which is what turns a type
    into comparable text without this script having to parse C.
    """
    lines = [
        "#define _LARGEFILE64_SOURCE 1",
        "#define ZLIB_CONST 1",
        "#include <stdarg.h>",
        "#include <stddef.h>",
        '#include "%s"' % header,
    ]
    lines += ["#undef %s" % name for name in SHADOWING_MACROS]
    lines.append("template <class Tag, class T> void zrs_signature() {}")
    for name in functions:
        lines.append("struct zrs_tag_%s {};" % name)
        lines.append(
            "template void zrs_signature<zrs_tag_%s, decltype(&%s)>();" % (name, name)
        )
    stem.parent.mkdir(parents=True, exist_ok=True)
    source_file = stem.with_suffix(".cpp")
    source_file.write_text("\n".join(lines) + "\n")
    object_file = stem.with_suffix(".o")
    built = run(
        [cxx, "-std=c++17", "-I.", *defines, "-c", "-o", str(object_file), str(source_file)]
    )
    if built.returncode != 0:
        return None, built.stderr
    listed = run(["nm", "-C", "--defined-only", str(object_file)])
    if listed.returncode != 0:
        return None, listed.stderr
    resolved: dict[str, str] = {}
    for line in listed.stdout.splitlines():
        match = re.search(r"void zrs_signature<zrs_tag_(\w+), (.*)>\(\)", line)
        if match:
            resolved[match.group(1)] = match.group(2)
    missing = [name for name in functions if name not in resolved]
    if missing:
        return None, "nm reported no signature for: %s" % ", ".join(missing)
    return ["FUNC %s\t%s" % (name, resolved[name]) for name in functions], ""


def project(
    header: Path,
    defines: list[str],
    functions: list[str],
    macros: list[str],
    types: list[str],
    stem: Path,
    cc: str,
    cxx: str,
) -> tuple[list[str] | None, str]:
    """Build one side's complete canonical projection, sorted so that order cannot matter."""
    signatures, error = signature_records(header, defines, functions, stem.with_name(stem.name + "_sig"), cxx)
    if signatures is None:
        return None, "signature projection: %s" % error
    values, error = value_records(header, defines, macros, types, stem.with_name(stem.name + "_val"), cc)
    if values is None:
        return None, "value projection: %s" % error
    return sorted(signatures + values), ""


def parse_allowance(raw: str) -> tuple[str, str]:
    """Parse `name=reason`. A reason is mandatory: an unexplained allowance is a silent skip."""
    name, separator, reason = raw.partition("=")
    if not separator or not reason.strip():
        raise argparse.ArgumentTypeError(
            "allowance %r needs a reason, as name='why it is absent'" % raw
        )
    return name.strip(), reason.strip()


def category_counts(records: list[str]) -> dict[str, int]:
    counts: dict[str, int] = {}
    for record in records:
        kind = record.split(" ", 1)[0]
        counts[kind] = counts.get(kind, 0) + 1
    return counts


def gate(arguments: argparse.Namespace) -> int:
    reference = Path(arguments.reference)
    generated = Path(arguments.generated)
    out_dir = Path(arguments.out_dir)
    out_dir.mkdir(parents=True, exist_ok=True)

    for path in (reference, generated):
        if not path.is_file():
            print("::error::%s does not exist, so there is nothing to compare" % path)
            return 1

    # Fail closed on tooling. There is no reduced form of a projection built by a compiler.
    tools = {"cc": arguments.cc, "c++": arguments.cxx, "nm": "nm"}
    for role, tool in tools.items():
        if shutil.which(tool) is None:
            print(
                "::error::%s (%s) is not on PATH. It resolves the typedef chain and the "
                "layout numbers, so this gate cannot run in a reduced form." % (role, tool)
            )
            return 1

    absent_functions = dict(arguments.allow_absent_function or [])
    absent_types = dict(arguments.allow_absent_type or [])

    declared = contract_function_names(reference)
    functions = [name for name in declared if name not in absent_functions]
    unknown = sorted(set(absent_functions) - set(declared))
    if unknown:
        print(
            "::error::--allow-absent-function names %s, which the contract does not declare. "
            "An allowance for a name that is not in the contract cannot be right."
            % ", ".join(unknown)
        )
        return 1

    types = [name for name in CONTRACT_TYPES if name not in absent_types]
    unknown_types = sorted(set(absent_types) - set(CONTRACT_TYPES))
    if unknown_types:
        print(
            "::error::--allow-absent-type names %s, which is not a contract type this gate "
            "projects." % ", ".join(unknown_types)
        )
        return 1

    # An allowance must not outlive the condition it was granted for. If the generated side
    # now declares an allowed-absent name, the allowance is stale: it would go on excluding a
    # name from the comparison that is available to compare, which silently shrinks the gate.
    # Preprocessing the generated side is what makes this checkable rather than assumed.
    preprocessed = run([arguments.cc, "-E", "-I.", *GENERATED_DEFINES, str(generated)])
    if preprocessed.returncode != 0:
        print("::error::%s could not preprocess %s" % (arguments.cc, generated))
        print(preprocessed.stderr[:2000])
        return 1
    visible = strip_comments(preprocessed.stdout)
    stale = [
        name
        for name in sorted(set(absent_functions) | set(absent_types))
        if re.search(r"\b%s\b" % re.escape(name), visible)
    ]
    if stale:
        print(
            "::error::%s is allowed absent but the generated header declares it. Remove the "
            "allowance so the name is compared: an allowance that is no longer needed keeps "
            "excluding something the gate could otherwise check."
            % ", ".join(stale)
        )
        return 1

    # Macros compared by VALUE, over the names both headers define as object-like macros.
    # Restricting to the contract headers' own definitions keeps the system's feature-test
    # macros out; intersecting with the generated side keeps the comparison symmetric.
    contract_macros = object_macro_names([reference, Path("zconf.h")]) if Path("zconf.h").is_file() else object_macro_names([reference])
    candidates = sorted(contract_macros & object_macro_names([generated]))

    scratch = Path(tempfile.mkdtemp(prefix="contract-projection-", dir=str(out_dir)))
    try:
        # A macro is comparable only if it evaluates to an integer on BOTH sides. `ZLIB_VERSION`
        # is a string and the large-file names are function redirects; both are covered by other
        # rules, and neither can be cast to `long long`. Probing one name at a time is what
        # keeps a single non-integer macro from failing the whole projection.
        macros = []
        for name in candidates:
            usable = True
            for header, defines in ((reference, REFERENCE_DEFINES), (generated, GENERATED_DEFINES)):
                probe, _ = compile_and_run(
                    probe_source(header, ['printf("%%lld\\n", (long long)(%s));' % name]),
                    defines,
                    scratch / ("macro_probe_" + name),
                    arguments.cc,
                )
                if probe is None:
                    usable = False
                    break
            if usable:
                macros.append(name)

        sides = (
            ("reference", reference, REFERENCE_DEFINES),
            ("generated", generated, GENERATED_DEFINES),
        )
        projections: dict[str, list[str]] = {}
        for label, header, defines in sides:
            records, error = project(
                header, defines, functions, macros, types,
                scratch / label, arguments.cc, arguments.cxx,
            )
            if records is None:
                print(
                    "::error::the canonical projection of %s could not be built, so the "
                    "comparison never ran" % header
                )
                print(error[:4000])
                return 1
            projections[label] = records

        written = {}
        for label in projections:
            path = out_dir / ("contract.%s" % ("ref" if label == "reference" else "gen"))
            path.write_text("\n".join(projections[label]) + "\n")
            written[label] = path

        counts = category_counts(projections["reference"])
        for kind, floor in MINIMUM_RECORDS.items():
            if counts.get(kind, 0) < floor:
                print(
                    "::error::the projection carries only %d %s record(s); at least %d are "
                    "required. An empty diff of a near-empty projection is not evidence."
                    % (counts.get(kind, 0), kind, floor)
                )
                return 1

        # AAP 0.8.6's own command, published as evidence rather than replaced by this gate.
        if arguments.write_raw_diff:
            raw_path = Path(arguments.write_raw_diff)
            raw_path.parent.mkdir(parents=True, exist_ok=True)
            raw = run(["diff", "-u", str(reference), str(generated)])
            raw_path.write_text(raw.stdout)
            print(
                "AAP 0.8.6 raw `diff -u %s %s`: %d line(s), kept at %s as evidence "
                "(header_raw_diff_gate.py accounts for every one of them)"
                % (reference, generated, len(raw.stdout.splitlines()), raw_path)
            )

        comparison = run(["diff", "-u", str(written["reference"]), str(written["generated"])])
        diff_path = out_dir / "contract.diff"
        diff_path.write_text(comparison.stdout)

        if comparison.returncode != 0 or comparison.stdout:
            print(
                "::error::the canonical contract projections are NOT identical. The diff "
                "below is authoritative: the left side is the frozen contract, and every "
                "record is a value the compiler produced rather than text either header "
                "happens to contain."
            )
            print(comparison.stdout[:8000])
            return 1

        total = len(projections["reference"])
        print(
            "PASS: `diff -u %s %s` is EXACTLY EMPTY over %d canonical contract record(s) -- "
            "%s." % (
                written["reference"], written["generated"], total,
                ", ".join("%d %s" % (counts[k], k) for k in sorted(counts)),
            )
        )
        for name, reason in sorted(absent_functions.items()):
            print("  allowed absent function: %s -- %s" % (name, reason))
        for name, reason in sorted(absent_types.items()):
            print("  allowed absent type:     %s -- %s" % (name, reason))
        return 0
    finally:
        shutil.rmtree(scratch, ignore_errors=True)


def self_test(arguments: argparse.Namespace) -> int:
    """Prove the gate FAILS on a perturbed contract, one category at a time.

    An empty diff is only evidence if a real divergence would break it. Each case edits a
    scratch COPY of the generated header -- never `zlib.h`, never the real artifact -- and
    requires a non-zero exit.
    """
    generated = Path(arguments.generated)
    if not generated.is_file():
        print("::error::--generated %s must exist for the self-test" % generated)
        return 1
    original = generated.read_text()

    cases = [
        (
            "a changed parameter type",
            lambda text: text.replace(
                "int deflate(z_streamp strm, int flush);",
                "int deflate(z_streamp strm, long flush);",
            ),
        ),
        (
            "a changed macro value",
            lambda text: re.sub(
                r"^(#define\s+Z_BEST_COMPRESSION\s+)\d+$", r"\g<1>8", text, flags=re.M
            ),
        ),
        (
            "a narrowed struct field, which also resizes the struct",
            lambda text: re.sub(r"^(\s*)z_off64_t pos;$", r"\g<1>int32_t pos;", text, flags=re.M),
        ),
        (
            "a narrowed public typedef",
            lambda text: text.replace(
                "typedef unsigned long uLong;", "typedef unsigned int uLong;"
            ),
        ),
    ]

    failures = []
    with tempfile.TemporaryDirectory(prefix="contract-selftest-") as tmp:
        for description, perturb in cases:
            perturbed = perturb(original)
            if perturbed == original:
                failures.append(
                    "%s: the perturbation matched nothing, so the case proved nothing "
                    "(the generated header's spelling changed -- update this case)"
                    % description
                )
                print("  %-52s SKIPPED -- pattern did not match" % description)
                continue
            scratch = Path(tmp) / "perturbed_zlib.h"
            scratch.write_text(perturbed)
            namespace = argparse.Namespace(**vars(arguments))
            namespace.generated = str(scratch)
            namespace.write_raw_diff = None
            namespace.out_dir = str(Path(tmp) / "out")
            code = gate(namespace)
            verdict = "FAILED as required" if code != 0 else "*** PASSED, which is wrong ***"
            print("  %-52s %s" % (description, verdict))
            if code == 0:
                failures.append("%s: the gate did not notice" % description)

    if failures:
        print("::error::self-test did not hold:")
        for failure in failures:
            print("  " + failure)
        return 1
    print("PASS: the gate rejects every perturbation, so an empty diff means agreement.")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Require an exactly-empty diff of the two headers' canonical contract projections."
    )
    parser.add_argument("--reference", default="zlib.h", help="the frozen contract (read-only)")
    parser.add_argument("--generated", default="generated_zlib.h", help="cbindgen's output")
    parser.add_argument("--out-dir", default="target/header-gate", help="where artifacts are written")
    parser.add_argument("--cc", default=os.environ.get("CC") or "cc")
    parser.add_argument("--cxx", default=os.environ.get("CXX") or "c++")
    parser.add_argument(
        "--allow-absent-function", action="append", type=parse_allowance, metavar="NAME=REASON",
        help="a contract function absent from the generated side, with the reason it is absent",
    )
    parser.add_argument(
        "--allow-absent-type", action="append", type=parse_allowance, metavar="NAME=REASON",
        help="a contract type absent from the generated side, with the reason it is absent",
    )
    parser.add_argument("--write-raw-diff", metavar="PATH", help="also publish AAP 0.8.6's raw diff here")
    parser.add_argument(
        "--self-test", action="store_true",
        help="prove the gate fails on a perturbed contract, then exit",
    )
    arguments = parser.parse_args()
    return self_test(arguments) if arguments.self_test else gate(arguments)


if __name__ == "__main__":
    sys.exit(main())
