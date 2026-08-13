#!/usr/bin/env python3
"""Type-check the retention compile-fail suite and require every case to fail as annotated.

`crates/zlib-rs-differential/src/retain.rs` turns three retention obligations into
type-system facts.  The tracking allocator's ledger, a ``gz_header`` and the
``inflateBack`` window are all addresses the library keeps *after* the call that
installed them returns, so each is lent to a ``retain::Session`` for the session's whole
life and drop-check refuses any arrangement where the storage would go first.

That claim -- "getting this wrong is a build failure and not a latent use-after-free" --
is only worth making if something checks it.  This is that check.  Every file in
``crates/zlib-rs-differential/tests/retention_compile_fail`` is a whole program carrying
two annotations::

    //@ error: E0597
    //@ proves: <one line, for the reader of a failing log>

The suite passes only when each case produces exactly the codes it declares, and when
the one case declaring ``none`` compiles cleanly.  That last one is what keeps the suite
honest: five cases failing for an unrelated reason -- a renamed method, a changed
signature -- would look identical to five cases failing for the right one, so the
control case turns "the API stopped type-checking at all" into a gate failure rather
than a silent pass.

The cases are not Cargo targets.  Cargo auto-discovers ``tests/*.rs`` and
``tests/*/main.rs``; this directory has neither, so ``cargo test``, ``cargo clippy`` and
``cargo fmt`` never see them.  ``rustc --emit=metadata`` is invoked directly instead,
which stops before linking and therefore needs none of the C oracle archive the crate
normally links -- so this gate runs anywhere the crate has been built once, and costs
about a second.

Usage -- no prior build needed, the script makes the one it checks against::

    python3 .github/scripts/retention_compile_fail.py             # debug profile
    python3 .github/scripts/retention_compile_fail.py --release   # the CI job's profile

Exit status is 0 when every case behaved and 1 otherwise, with every failure named.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
import tempfile
from dataclasses import dataclass
from pathlib import Path

#: Where the cases live, relative to the repository root.
CASE_DIR = Path("crates/zlib-rs-differential/tests/retention_compile_fail")

#: `crate name -> the file name Cargo writes for its rlib`.
#:
#: ★ EXACTLY ONE ENTRY, and that is a correctness requirement rather than economy.  A case
#: names only the crate under test and lets `rustc` resolve everything else from that
#: crate's metadata through `-L dependency=`, so a case can never be type-checked against
#: a *differently configured* build of a dependency.  Measured: adding
#: `libz_rs_sys=<profile>/libz.rlib` here made three cases fail with `E0308` under the
#: release profile, because that profile holds two `libz.rlib` spellings and only one of
#: them is the crate the harness was compiled against.  Anything a case needs from a
#: dependency -- `gz_header`, for instance -- is reached through a gate the harness
#: re-exports, such as `port::zeroed_header()`.
EXTERNS = {
    "zlib_rs_differential": "libzlib_rs_differential.rlib",
}

#: `//@ error: E0597, E0502` or `//@ error: none`.
ERROR_DIRECTIVE = re.compile(r"^//@\s*error:\s*(?P<codes>.+?)\s*$")

#: `//@ proves: <one line>`.
PROVES_DIRECTIVE = re.compile(r"^//@\s*proves:\s*(?P<text>\S.*?)\s*$")

#: Any `error[E0000]` header rustc prints.
EMITTED_CODE = re.compile(r"^error\[(?P<code>E\d{4})\]", re.MULTILINE)

#: A bare `error:` header, which carries no code.  Distinguished from the coded form so a
#: case that fails for a reason rustc does not classify is reported as such rather than
#: silently counting as "no codes".
UNCODED_ERROR = re.compile(r"^error:\s*(?P<text>.+)$", re.MULTILINE)

#: rustc's own summary line, which is an uncoded error but never the cause of one.
SUMMARY_ERROR = re.compile(r"^aborting due to \d+ previous error")


@dataclass(frozen=True)
class Case:
    """One compile-fail case: its path, the codes it must produce, and what it proves."""

    path: Path
    expected: frozenset[str]
    proves: str

    @property
    def name(self) -> str:
        """The case's file name, which is how every message refers to it."""
        return self.path.name

    @property
    def is_control(self) -> bool:
        """Whether this case must *compile* rather than fail."""
        return not self.expected


def parse_case(path: Path) -> Case:
    """Read one case's annotations, or raise ``ValueError`` naming what is wrong."""
    codes: frozenset[str] | None = None
    proves: str | None = None
    for line in path.read_text(encoding="utf-8").splitlines():
        stripped = line.strip()
        if not stripped.startswith("//@"):
            # Directives must precede any code; a blank line or a doc comment is fine.
            if stripped and not stripped.startswith("//"):
                break
            continue
        matched = ERROR_DIRECTIVE.match(stripped)
        if matched is not None:
            raw = matched.group("codes")
            if raw.strip().lower() == "none":
                codes = frozenset()
            else:
                parsed = {token.strip() for token in raw.split(",") if token.strip()}
                unknown = sorted(c for c in parsed if not re.fullmatch(r"E\d{4}", c))
                if unknown:
                    raise ValueError(
                        f"{path.name}: `//@ error:` names {', '.join(unknown)}, which is "
                        f"not an `E0000`-shaped rustc code"
                    )
                if not parsed:
                    raise ValueError(f"{path.name}: `//@ error:` is empty")
                codes = frozenset(parsed)
            continue
        matched = PROVES_DIRECTIVE.match(stripped)
        if matched is not None:
            proves = matched.group("text")
            continue
        raise ValueError(f"{path.name}: unrecognised directive {stripped!r}")

    if codes is None:
        raise ValueError(f"{path.name}: no `//@ error:` directive")
    if proves is None:
        raise ValueError(f"{path.name}: no `//@ proves:` directive")
    return Case(path=path, expected=codes, proves=proves)


def build_library(root: Path, release: bool) -> Path:
    """Build the harness library and return the exact rlib Cargo says it produced.

    ★ ASK CARGO, DO NOT GUESS.  Locating the rlib by name was tried first and is not
    sound, in two measured ways:

    * Cargo "uplifts" an unhashed copy into the profile root for some targets in some
      profiles and not others.  ``zlib-rs-differential`` is uplifted under ``debug`` and
      **not** under ``release``, so looking only for ``target/release/libzlib_rs_
      differential.rlib`` made an earlier version of this gate refuse to run in the very
      profile the CI job uses.
    * Choosing the newest candidate by modification time does not fix it.  ``cargo test
      --lib`` builds the library *as a test binary* and can leave every plain rlib in the
      tree older than the source, with mtimes that still look fresh, so the cases were
      type-checked against metadata several edits old and reported `E0432` on a gate that
      had just been made to pass.

    A gate that silently checks stale metadata is worse than no gate, so this builds the
    library itself -- a no-op in CI, where the job has already compiled it -- and reads the
    artifact path out of Cargo's own JSON.  ``--locked`` keeps it offline.
    """
    command = [
        os.environ.get("CARGO", "cargo"),
        "build",
        "--locked",
        "--quiet",
        "-p",
        "zlib-rs-differential",
        "--lib",
        "--message-format=json",
    ]
    if release:
        command.append("--release")
    completed = subprocess.run(  # noqa: S603 -- fixed argv, no shell
        command,
        cwd=root,
        capture_output=True,
        text=True,
        check=False,
        # Cargo's target-info probe runs `rustc -` with INHERITED stdin and would read
        # whatever the parent's stdin happens to carry as program text, then cache the
        # resulting error.  Closing it is the fix, and it costs nothing.
        stdin=subprocess.DEVNULL,
    )
    if completed.returncode != 0:
        sys.exit(
            f"error: `{' '.join(command[1:6])} ...` failed, so there is nothing to check "
            f"the cases against:\n{completed.stderr.strip()}"
        )

    rlib: Path | None = None
    for line in completed.stdout.splitlines():
        try:
            message = json.loads(line)
        except json.JSONDecodeError:
            continue
        if message.get("reason") != "compiler-artifact":
            continue
        # Matched on the MANIFEST rather than the target name: Cargo reports a lib
        # target under its underscored name (`zlib_rs_differential`), not the package's
        # hyphenated one, and matching the package name silently found nothing.
        manifest = str(message.get("manifest_path") or "")
        if not manifest.endswith(str(Path("crates/zlib-rs-differential/Cargo.toml"))):
            continue
        target = message.get("target") or {}
        if "lib" not in target.get("kind", []):
            continue
        for filename in message.get("filenames") or []:
            if filename.endswith(".rlib"):
                rlib = Path(filename)
    if rlib is None or not rlib.is_file():
        sys.exit(
            "error: cargo reported no rlib for zlib-rs-differential's lib target.\n"
            "       That should be impossible after a successful build; re-run with\n"
            "       `cargo build -p zlib-rs-differential --lib --message-format=json`\n"
            "       to see what it did report."
        )
    return rlib


def extern_arguments(rlib: Path) -> list[str]:
    """The single `--extern` flag, plus the `-L` paths that let rustc find its dependencies.

    Cargo reports either the uplifted rlib in the profile root or the hashed one in
    ``deps/``, so both directories are offered as search paths and the ones that do not
    exist are dropped.  Offering only the reported rlib's own parent produced `E0463` for
    every case in the profile where the uplift is what Cargo names: the harness's
    dependencies are in ``deps/`` regardless of where the harness itself was reported.
    """
    parent = rlib.parent
    search = [parent, parent / "deps", parent.parent] if parent.name == "deps" else [
        parent,
        parent / "deps",
    ]
    arguments = ["--extern", f"{next(iter(EXTERNS))}={rlib}"]
    seen: set[Path] = set()
    for directory in search:
        if directory.is_dir() and directory not in seen:
            seen.add(directory)
            arguments += ["-L", f"dependency={directory}"]
    return arguments


def emitted_codes(stderr: str) -> tuple[frozenset[str], list[str]]:
    """The coded errors rustc emitted, and any uncoded ones bar its own summary line."""
    codes = frozenset(match.group("code") for match in EMITTED_CODE.finditer(stderr))
    uncoded = [
        match.group("text")
        for match in UNCODED_ERROR.finditer(stderr)
        if SUMMARY_ERROR.match(match.group("text")) is None
    ]
    return codes, uncoded


def check(case: Case, root: Path, externs: list[str]) -> list[str]:
    """Type-check one case.  Returns the reasons it failed the gate, empty when it passed."""
    with tempfile.TemporaryDirectory(prefix="retention-compile-fail-") as scratch:
        completed = subprocess.run(  # noqa: S603 -- fixed argv, no shell
            [
                os.environ.get("RUSTC", "rustc"),
                "--edition",
                "2021",
                "--emit=metadata",
                "--crate-type",
                "bin",
                "-o",
                str(Path(scratch) / "case.rmeta"),
                *externs,
                str(case.path),
            ],
            cwd=root,
            capture_output=True,
            text=True,
            check=False,
            stdin=subprocess.DEVNULL,
        )

    codes, uncoded = emitted_codes(completed.stderr)
    reasons: list[str] = []

    if case.is_control:
        if completed.returncode != 0:
            reasons.append(
                "the CONTROL case does not compile, so the negative cases below prove "
                "nothing about retention -- they would fail for this reason too. "
                f"rustc said: {'; '.join(sorted(codes) + uncoded) or 'see the transcript'}"
            )
        return reasons

    if completed.returncode == 0:
        reasons.append(
            f"compiled cleanly, but must be refused with {', '.join(sorted(case.expected))} "
            f"-- the retention obligation it violates is no longer enforced"
        )
        return reasons

    missing = sorted(case.expected - codes)
    if missing:
        reasons.append(
            f"did not produce {', '.join(missing)}; rustc produced "
            f"{', '.join(sorted(codes)) or 'no coded error'}"
        )
    unexpected = sorted(codes - case.expected)
    if unexpected:
        reasons.append(
            f"also produced {', '.join(unexpected)}, so it is failing for a reason the "
            f"annotation does not describe"
        )
    if uncoded:
        reasons.append(
            f"produced an uncoded error, which no retention violation does: {uncoded[0]}"
        )
    return reasons


def main() -> int:
    """Run every case, print one line each, and fail on any deviation."""
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument(
        "--root",
        default=str(Path(__file__).resolve().parents[2]),
        help="repository root (default: two directories above this script)",
    )
    parser.add_argument(
        "--release",
        action="store_true",
        help="build and check against the release profile (the `differential` job's profile)",
    )
    arguments = parser.parse_args()

    root = Path(arguments.root).resolve()
    case_dir = root / CASE_DIR
    if not case_dir.is_dir():
        sys.exit(f"error: {CASE_DIR} does not exist under {root}")

    paths = sorted(case_dir.glob("*.rs"))
    if not paths:
        sys.exit(f"error: {CASE_DIR} holds no cases, so this gate would pass vacuously")

    try:
        cases = [parse_case(path) for path in paths]
    except ValueError as problem:
        sys.exit(f"error: {problem}")

    controls = [case for case in cases if case.is_control]
    if len(controls) != 1:
        sys.exit(
            f"error: expected exactly one `//@ error: none` control case, found "
            f"{len(controls)} -- without one the suite can pass vacuously, and with two "
            f"it is ambiguous which arrangement is the supported one"
        )

    rlib = build_library(root, arguments.release)
    externs = extern_arguments(rlib)
    try:
        shown = rlib.relative_to(root)
    except ValueError:
        shown = rlib
    print(f"retention compile-fail suite: {len(cases)} case(s), checked against {shown}")

    failures = 0
    for case in cases:
        reasons = check(case, root, externs)
        expectation = "compiles" if case.is_control else ", ".join(sorted(case.expected))
        if reasons:
            failures += 1
            print(f"  FAIL {case.name} [{expectation}]")
            for reason in reasons:
                print(f"       {reason}")
        else:
            print(f"  ok   {case.name} [{expectation}] -- {case.proves}")

    if failures:
        print(
            f"\nerror: {failures} of {len(cases)} retention cases did not behave as annotated.\n"
            f"       Each case is a whole program whose only defect is one of the mistakes\n"
            f"       crates/zlib-rs-differential/src/retain.rs exists to prevent. A case that\n"
            f"       compiles means that prevention is gone; a case failing with the wrong code\n"
            f"       means it is being caught for an unrelated reason and the guarantee is\n"
            f"       untested. Reproduce locally with:\n"
            f"         python3 .github/scripts/retention_compile_fail.py"
        )
        return 1

    print(f"\nPASS: all {len(cases)} retention cases behaved as annotated.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
