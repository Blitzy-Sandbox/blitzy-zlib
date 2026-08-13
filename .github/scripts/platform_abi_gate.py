#!/usr/bin/env python3
"""Compare a native shared library's measured export table against the frozen contract.

WHY THIS EXISTS.  The `symbols` and `dropin` jobs measure the export surface and relink
the C drivers on ubuntu-latest alone, so the macOS and Windows rows of `build-test`
established only that the workspace compiles and its tests pass.  Nothing off Linux
looked at a PACKAGED artifact: not its install name, not its export table, not whether
an UNMODIFIED C consumer links against it, not whether the loader binds it rather than
the platform's own zlib.  AAP 0.8.3 requires the packaged library to be consumable, and
AAP 0.6.3.4 requires both large-file symbol families to be exported; neither was checked
on a non-ELF format.

WHAT IT COMPARES, AND WHERE EACH SIDE COMES FROM.  Both sides are derived from the tree,
never hardcoded here:

  * the contract  -- every ZEXTERN declaration in `zlib.h`, which AAP 0.2.1.2 makes
    immutable.  96 names on this tree.
  * the internals -- every name inside a `local:` section of `zlib.map`, plus that
    section's `_*` wildcard.  These are the names a consumer must NOT be able to reach.
  * the exports   -- measured on the runner by the platform's own tool (`nm -gU` for
    Mach-O, `llvm-readobj --coff-exports` for PE, `nm -D` for ELF) and handed here as
    one name per line.

The caller passes `--platform` because the three formats do not offer the same
guarantees, and pretending they do is how a gate ends up asserting something it cannot
see.  ELF applies `zlib.map`, so hiding is enforced and a leaked internal is fatal.
Mach-O and PE have no version script -- `crates/libz-rs-sys/build.rs` records the
analogues (an exported-symbols list, and `win32/zlib.def`, which AAP 0.2.2.3 puts out of
scope) -- so on those formats this gate reports a reachable internal as UNHIDDEN and does
not fail on it, while still failing on any export it cannot account for at all.

FAIL-CLOSED.  A missing or empty export list, a contract scan that finds implausibly few
names, an internals scan that finds none, or any exported name that is neither in the
contract nor classifiable as an internal, all fail.  An expected-absent name is only
tolerated where this file records WHY, with its citation.
"""

from __future__ import annotations

import argparse
import fnmatch
import re
import sys
from pathlib import Path

# A ZEXTERN declaration, matched as a STATEMENT rather than by anchoring the line.  The
# large-file names are declared more than once under different `#if` branches, so the set
# of names is smaller than the number of statements; that is expected and is why the
# comparison is over a set.
ZEXTERN_STATEMENT = re.compile(r"\bZEXTERN\b[^;]*;", re.S)

# Tokens that can precede the declared name inside a statement.  `OF` is zlib's own
# pre-C89 parameter-list macro and takes a parenthesised list, so it looks exactly like a
# function name to a naive scan.
NOT_A_NAME = frozenset({"ZEXTERN", "ZEXPORT", "ZEXPORTVA", "FAR", "OF", "z_const", "const"})

# Names the contract declares that a given platform's artifact may legitimately not
# export.  Every entry carries the reason, and the reason is printed, so widening this
# table is visible in the log rather than silent.  A name listed here is tolerated in
# EITHER direction: the gate reports whether it was in fact present.
PLATFORM_ABSENCES: dict[str, dict[str, str]] = {
    "linux": {
        "gzopen_w": (
            "declared only under _WIN32 (zlib.h L2042) and implemented `#[cfg(windows)]`, "
            "so it is not a symbol a Linux build can export"
        ),
    },
    "macos": {
        "gzopen_w": (
            "declared only under _WIN32 (zlib.h L2042) and implemented `#[cfg(windows)]`, "
            "so it is not a symbol a macOS build can export"
        ),
    },
    "windows": {
        # These two are the C shim's, not Rust's: gzprintf is variadic and gzvprintf takes
        # a va_list, neither of which stable Rust can define, so both live in
        # csrc/gzprintf_shim.c.  crates/libz-rs-sys/build.rs records that a rustc-linked
        # cdylib cannot re-export a symbol arriving from a native archive, and the PE
        # analogue of zlib.map -- win32/zlib.def -- is out of scope per AAP 0.2.2.3.  The
        # ELF drop-in gets them because `make rust` relinks the whole archive; nothing
        # equivalent is wired for PE, so their absence here is expected rather than new.
        "gzprintf": (
            "lives in csrc/gzprintf_shim.c (variadic; stable Rust cannot define it) and a "
            "rustc-linked cdylib cannot re-export a native-archive symbol -- see "
            "crates/libz-rs-sys/build.rs; the PE analogue of zlib.map (win32/zlib.def) is "
            "out of scope per AAP 0.2.2.3"
        ),
        "gzvprintf": (
            "lives in csrc/gzprintf_shim.c (takes a va_list) and a rustc-linked cdylib "
            "cannot re-export a native-archive symbol -- see crates/libz-rs-sys/build.rs; "
            "the PE analogue of zlib.map (win32/zlib.def) is out of scope per AAP 0.2.2.3"
        ),
    },
}

# Whether the format applies zlib.map, and therefore whether a reachable internal is a
# failure or a reported limitation.  ELF does; the other two have no version script, which
# build.rs documents rather than works around.
HIDING_ENFORCED = {"linux": True, "macos": False, "windows": False}


def contract_names(path: Path) -> set[str]:
    """Every function name zlib.h declares with ZEXTERN."""
    text = re.sub(r"/\*.*?\*/", " ", path.read_text(encoding="utf-8", errors="replace"), flags=re.S)
    names: set[str] = set()
    for statement in ZEXTERN_STATEMENT.findall(text):
        for candidate in re.findall(r"(\w+)\s*\(", statement):
            if candidate not in NOT_A_NAME:
                names.add(candidate)
                break
    return names


def internal_names(path: Path) -> tuple[set[str], list[str]]:
    """The exact names and the wildcard patterns inside zlib.map's `local:` sections."""
    exact: set[str] = set()
    patterns: list[str] = []
    in_local = False
    for line in path.read_text(encoding="utf-8", errors="replace").splitlines():
        stripped = line.strip()
        if stripped.startswith("local:"):
            in_local = True
            continue
        if stripped.startswith("global:") or stripped.startswith("}"):
            in_local = False
            continue
        if not in_local:
            continue
        token = stripped.rstrip(";").strip()
        if not token:
            continue
        if "*" in token or "?" in token:
            patterns.append(token)
        else:
            exact.add(token)
    return exact, patterns


def main() -> int:
    """Compare one measured export table against the contract and report exactly."""
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--platform", required=True, choices=sorted(PLATFORM_ABSENCES))
    parser.add_argument("--exports", default=None, help="measured export names, one per line")
    parser.add_argument("--contract", default="zlib.h", help="the immutable contract header")
    parser.add_argument(
        "--emit-exported-symbols-list",
        default=None,
        metavar="PATH",
        help=(
            "instead of comparing, write ld64's -exported_symbols_list: the contract names "
            "that --defined-symbols says the archive actually defines, each with the Mach-O "
            "leading underscore. This is the Mach-O analogue of zlib.map, DERIVED from "
            "zlib.h rather than written by hand, and intersecting with the defined set is "
            "what keeps the list from naming a symbol that does not exist."
        ),
    )
    parser.add_argument(
        "--defined-symbols",
        default=None,
        metavar="FILE",
        help="symbols the archive defines, one per line, underscore already stripped",
    )
    parser.add_argument("--version-script", default="zlib.map", help="the internals list")
    parser.add_argument(
        "--artifact",
        default=None,
        help="the library the export list was measured from, for the log",
    )
    arguments = parser.parse_args()

    if arguments.emit_exported_symbols_list:
        if not arguments.defined_symbols:
            sys.exit("error: --emit-exported-symbols-list needs --defined-symbols")
        defined_path = Path(arguments.defined_symbols)
        if not defined_path.is_file():
            sys.exit(f"error: {defined_path} does not exist, so the archive's defined "
                     f"symbols are unknown and the list would name symbols blindly")
        defined = {line.strip() for line in defined_path.read_text(encoding="utf-8").splitlines()}
        defined.discard("")
        contract = contract_names(Path(arguments.contract))
        if len(contract) < 90:
            sys.exit(f"error: only {len(contract)} ZEXTERN declarations found in "
                     f"{arguments.contract}; the contract scan is wrong")
        exported = sorted(contract & defined)
        absences = PLATFORM_ABSENCES[arguments.platform]
        unexplained = sorted(contract - defined - set(absences))
        if unexplained:
            sys.exit(
                f"error: the archive defines {len(unexplained)} fewer contract function(s) "
                f"than it must: {', '.join(unexplained)}. An exported-symbols list cannot "
                f"paper over a symbol the archive does not contain."
            )
        if len(exported) < 90:
            sys.exit(f"error: the list would carry only {len(exported)} name(s); that is not "
                     f"a plausible export surface for this contract")
        Path(arguments.emit_exported_symbols_list).write_text(
            "".join(f"_{name}\n" for name in exported), encoding="utf-8"
        )
        print(
            f"wrote {arguments.emit_exported_symbols_list}: {len(exported)} contract name(s) "
            f"the archive defines"
            + (f", {len(contract) - len(exported)} expected absence(s): "
               f"{', '.join(sorted(contract - defined))}" if contract - defined else "")
        )
        return 0

    if not arguments.exports:
        sys.exit("error: --exports is required unless --emit-exported-symbols-list is given")
    exports_path = Path(arguments.exports)
    if not exports_path.is_file():
        sys.exit(
            f"error: {exports_path} does not exist. The measurement step did not run or "
            f"did not write its product, and a gate that cannot see the export table must "
            f"not pass."
        )
    exports = {line.strip() for line in exports_path.read_text(encoding="utf-8").splitlines()}
    exports.discard("")
    if not exports:
        sys.exit(
            f"error: {exports_path} is empty. Either the artifact exports nothing -- which "
            f"has happened on this project, from a link that produced a zero-export library "
            f"-- or the extraction command printed a format this step did not parse. Both "
            f"are failures."
        )

    contract = contract_names(Path(arguments.contract))
    if len(contract) < 90:
        sys.exit(
            f"error: only {len(contract)} ZEXTERN declarations found in {arguments.contract}; "
            f"the contract scan is wrong, and a scan that finds nothing would make this "
            f"comparison vacuous."
        )
    internals, wildcards = internal_names(Path(arguments.version_script))
    if not internals:
        sys.exit(
            f"error: no names found in a `local:` section of {arguments.version_script}; the "
            f"internals scan is wrong, so a leaked internal could not be recognised."
        )

    absences = PLATFORM_ABSENCES[arguments.platform]
    artifact = arguments.artifact or "the measured artifact"
    print(
        f"{artifact}: {len(exports)} exported name(s) measured on {arguments.platform}, "
        f"against {len(contract)} contract declaration(s) in {arguments.contract} and "
        f"{len(internals)} internal(s) plus {len(wildcards)} wildcard(s) in "
        f"{arguments.version_script}"
    )

    missing = sorted(contract - exports)
    unexplained = [name for name in missing if name not in absences]
    for name in missing:
        if name in absences:
            print(f"  absent, and expected to be: {name} -- {absences[name]}")
    for name in sorted(absences):
        if name in exports:
            print(f"  present, though this platform is allowed to omit it: {name}")
    if unexplained:
        print(
            f"\nerror: {artifact} does not export {len(unexplained)} contract function(s): "
            f"{', '.join(unexplained)}.\n"
            f"       zlib.h is the immutable contract (AAP 0.2.1.2), so a name it declares "
            f"and the packaged library does not is a symbol a consumer will fail to link "
            f"against without recompiling -- which is the one thing this port promises will "
            f"not happen. If one of these genuinely cannot exist on this platform, it needs "
            f"an entry in PLATFORM_ABSENCES with its reason, not a wider comparison."
        )
        return 1

    print(
        f"  contract: all {len(contract) - len(missing)} of {len(contract)} declared "
        f"function(s) are exported"
        + (f" ({len(missing)} expected absence(s))" if missing else "")
    )

    extra = sorted(exports - contract)
    leaked_exact = [name for name in extra if name in internals]
    leaked_wild = [
        name
        for name in extra
        if name not in internals and any(fnmatch.fnmatch(name, p) for p in wildcards)
    ]
    unaccounted = [name for name in extra if name not in leaked_exact and name not in leaked_wild]

    if leaked_exact or leaked_wild:
        reachable = sorted(leaked_exact + leaked_wild)
        if HIDING_ENFORCED[arguments.platform]:
            print(
                f"\nerror: {artifact} exports {len(reachable)} name(s) that "
                f"{arguments.version_script} places in a `local:` section: "
                f"{', '.join(reachable)}.\n"
                f"       This format applies the version script, so hiding is enforced here "
                f"and a reachable internal is a real ABI difference from the C library."
            )
            return 1
        print(
            f"  UNHIDDEN, reported not failed: {len(reachable)} internal(s) reachable -- "
            f"{', '.join(reachable)}"
        )
        print(
            f"       {arguments.platform} has no version script. build.rs records the "
            f"analogues (an exported-symbols list for Mach-O; win32/zlib.def for PE, out of "
            f"scope per AAP 0.2.2.3), so this is a known limitation of the packaged artifact "
            f"on this format and not a regression. It is printed every run so it cannot "
            f"become invisible."
        )

    if unaccounted:
        print(
            f"\nerror: {artifact} exports {len(unaccounted)} name(s) that are neither in the "
            f"contract nor listed as internal by {arguments.version_script}: "
            f"{', '.join(unaccounted[:40])}.\n"
            f"       AAP 0.8.1 fixes the exported surface at the existing declarations -- no "
            f"additions. An export accounted for by nothing in the tree is either a new "
            f"surface the contract never promised or a symbol that escaped, and both have to "
            f"be resolved rather than tolerated."
        )
        return 1

    print(
        f"\nPASS: every contract function this platform can export is exported by {artifact}, "
        f"and every exported name is accounted for by zlib.h or zlib.map."
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
