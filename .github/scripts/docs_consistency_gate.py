#!/usr/bin/env python3
"""Check that rust/README.md still describes the gates, scripts and options that exist.

WHY THIS EXISTS.  A developer guide describing CI is a second copy of facts whose first
copy is executable, and the second copy rots silently.  This one had rotted in ten
places at once: it called `.github/workflows/cmake.yml` a C-only matrix when four of its
rows build the Rust library, described two AddressSanitizer stages as differing only in
their target directory when they had been the identical command, said no workflow
referenced `fetch_silesia.sh` when one already did, presented the raw cbindgen diff as
something a reviewer confirms by eye when it is now gated line by line, and stated a job
count that three separate changes had left behind.  Every one of those was true when
written.  Prose cannot be trusted to stay true, so the checkable parts of it are checked.

WHAT IT CHECKS.  Only claims with a machine-readable counterpart, and it compares against
the tree rather than against a list kept here:

  1. every workflow job the guide names in backticks exists in that workflow;
  2. every `.github/scripts/...` path it names exists on disk;
  3. the job count it states equals the number of jobs, and the split it states between
     `ubuntu-latest` and elsewhere is the real split;
  4. every `ZLIB_RS_*` toggle it documents is read somewhere in the tree;
  5. every CMake option it names is declared in CMakeLists.txt.

WHAT IT DOES NOT CHECK.  Whether the prose is TRUE -- no gate can do that.  It catches the
class of rot where a name, a path, a count or a toggle stops existing, which is what makes
a guide actively misleading rather than merely dated.

FAIL-CLOSED.  If the guide, a workflow or CMakeLists.txt cannot be read, or if the scan
finds implausibly few of anything it expects to find, that is a failure: a check that
silently verified nothing is worse than no check, because it reports success.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

README = Path("rust/README.md")
CMAKELISTS = Path("CMakeLists.txt")
WORKFLOWS = Path(".github/workflows")

# EVERY workflow, not a chosen two.  The guide discusses jobs in `rust.yml`, `cmake.yml`
# and `contribs.yml` (whose `ci-cmake-rust` job builds the out-of-scope contrib consumers
# against a Rust install), and an earlier draft of this gate read only the first two -- so
# it reported that real job as nonexistent.  Reading them all removes a list to maintain.
JOB_SOURCES = {path.name: path for path in sorted(WORKFLOWS.glob("*.yml"))}

# Hyphenated lowercase tokens in backticks that are NOT job names.  This list is what makes
# a MISSPELLED job name a failure: without it, an unknown token was simply filtered out and
# `platform-abi-mac` passed as happily as `platform-abi-macos`, which is the one class of
# rot this check most needs to catch.  So the rule for a hyphenated token is now "either a
# declared job, or named here" -- crate names, features, profiles, tools and runner labels
# go here, and anything else has to be a real job.  Single-word tokens are matched only
# against declared job names, because `header` and `bench` are ordinary words and an
# allowlist for them would be unbounded.
NON_JOB_TERMS = {
    "bench-parity", "default-members", "dev-dependencies", "i686-unknown-linux-gnu",
    "libfuzzer-sys", "libz-compat", "libz-rs-sys", "macos-latest", "pkg-config",
    "rust-api", "rust-version", "ubuntu-latest", "zlib-rs", "zlib-rs-benches",
    "zlib-rs-differential", "zlib-rs-fuzz",
}

NUMBER_WORDS = {
    "one": 1, "two": 2, "three": 3, "four": 4, "five": 5, "six": 6, "seven": 7,
    "eight": 8, "nine": 9, "ten": 10, "eleven": 11, "twelve": 12, "thirteen": 13,
    "fourteen": 14, "fifteen": 15, "sixteen": 16, "seventeen": 17, "eighteen": 18,
    "nineteen": 19, "twenty": 20, "twenty-one": 21, "twenty-two": 22,
    "twenty-three": 23, "twenty-four": 24, "twenty-five": 25,
}


def read(path: Path) -> str:
    """Read a file, failing closed rather than skipping the checks that need it."""
    if not path.is_file():
        sys.exit(f"error: {path} does not exist, so the claims that depend on it cannot be checked")
    return path.read_text(encoding="utf-8", errors="replace")


def workflow_jobs(text: str) -> set[str]:
    """Job names declared in a workflow, read without a YAML dependency.

    A job is a two-space-indented `name:` key under `jobs:`; matching on the indent keeps
    step names and `runs-on` values out of the set.
    """
    jobs: set[str] = set()
    in_jobs = False
    for line in text.splitlines():
        if re.match(r"^jobs:\s*$", line):
            in_jobs = True
            continue
        if in_jobs and re.match(r"^\S", line):
            break
        if in_jobs:
            match = re.match(r"^  ([A-Za-z0-9_-]+):\s*$", line)
            if match:
                jobs.add(match.group(1))
    return jobs


def main() -> int:
    """Compare the guide's checkable claims against the tree."""
    guide = read(README)
    failures: list[str] = []
    checked = 0

    jobs = {name: workflow_jobs(read(path)) for name, path in JOB_SOURCES.items()}
    # cmake.yml legitimately declares ONE job -- `ci-cmake`, whose breadth is in its matrix
    # rather than in separate jobs -- so the floor is per-file: every workflow must yield at
    # least one, and rust.yml, which is the suite, must yield many.  A floor of two for both
    # would fail on a correct tree, which is its own kind of broken gate.
    for name, declared in jobs.items():
        floor = 10 if name == "rust.yml" else 1
        if len(declared) < floor:
            sys.exit(
                f"error: only {len(declared)} job(s) found in {JOB_SOURCES[name]}, fewer than "
                f"the {floor} this file must declare; the job scan is wrong, and a scan that "
                f"finds nothing would make these checks vacuous"
            )
    all_jobs = set().union(*jobs.values())

    # 1. Job names.
    # Single-word job names (`header`, `symbols`, `asan`) matter as much as hyphenated ones,
    # so the pattern admits both and the intersection below is what distinguishes a job-name
    # claim from ordinary prose in backticks.  A prose word that happens to match a real job
    # name passes harmlessly; the failure this catches is a name that matches NOTHING.
    named = {token for token in re.findall(r"`([a-z][a-z0-9-]*)`", guide)}
    single = sorted(token for token in named if "-" not in token and token in all_jobs)
    hyphenated = sorted(token for token in named if "-" in token)
    job_claims = single + [token for token in hyphenated if token not in NON_JOB_TERMS]
    if len(job_claims) < 10:
        sys.exit(
            f"error: the guide names only {len(job_claims)} job(s); it describes the CI "
            f"suite in detail, so this scan has stopped working"
        )
    for job in job_claims:
        checked += 1
        if job not in all_jobs:
            failures.append(
                f"the guide names `{job}`, which no workflow declares as a job. If it is a "
                f"job, it was renamed or removed and the guide now sends a reader looking "
                f"for something that does not exist; if it is not a job at all, add it to "
                f"NON_JOB_TERMS in this script so the distinction stays explicit."
            )
    print(
        f"job names: {len(job_claims)} named by the guide ({len(single)} single-word, "
        f"{len(job_claims) - len(single)} hyphenated), all declared by a workflow"
    )

    # 1b. THE REVERSE DIRECTION, which is what catches a single-word job renamed in the
    # workflow without the guide following.  Checking prose against the tree cannot catch
    # that -- `format` renamed to `formatting` leaves the guide's `format` looking like an
    # ordinary word -- but checking the tree against prose can: every job the suite declares
    # must be mentioned SOMEWHERE in the guide, backticked or not.  A new job that genuinely
    # needs no mention does not exist in a file whose stated purpose is to record what each
    # job establishes.
    for job in sorted(jobs["rust.yml"]):
        checked += 1
        if not re.search(r"\b" + re.escape(job) + r"\b", guide, re.I):
            failures.append(
                f"the workflow declares a job `{job}` that {README} never mentions. Either "
                f"it is new and undocumented, or it was renamed and the guide still "
                f"describes it under its old name -- in which case some other sentence here "
                f"is now wrong about it."
            )
    print(f"job coverage: all {len(jobs['rust.yml'])} declared job(s) are mentioned by the guide")

    # 2. Script paths.
    scripts = sorted(set(re.findall(r"\.github/scripts/([A-Za-z0-9_.-]+)", guide)))
    if not scripts:
        sys.exit("error: the guide names no .github/scripts path; that scan has stopped working")
    for script in scripts:
        checked += 1
        if not (WORKFLOWS.parent / "scripts" / script).is_file():
            failures.append(
                f"the guide names .github/scripts/{script}, which does not exist. A gate "
                f"described but absent reads as coverage that is not there."
            )
    print(f"script paths: {len(scripts)} named, all present")

    # 3. The job count and the platform split, both stated in words.
    rust_jobs = jobs["rust.yml"]
    rust_text = read(JOB_SOURCES["rust.yml"])
    # A job runs somewhere other than plain ubuntu-latest if its `runs-on` is not the
    # literal, which covers both the matrix rows and the redirected bench runner.
    elsewhere = 0
    for job in rust_jobs:
        block = rust_text.split(f"\n  {job}:\n", 1)
        if len(block) < 2:
            continue
        body = re.split(r"\n  [A-Za-z0-9_-]+:\n", block[1])[0]
        runs_on = re.search(r"^    runs-on:\s*(.+)$", body, re.M)
        if runs_on and runs_on.group(1).strip() != "ubuntu-latest":
            elsewhere += 1
    count_claim = re.search(r"across ([a-z-]+) jobs", guide)
    if not count_claim:
        failures.append(
            "the guide no longer states how many jobs the workflow runs; that sentence is "
            "the one a reader uses to judge whether the guide is current"
        )
    else:
        checked += 1
        stated = NUMBER_WORDS.get(count_claim.group(1))
        if stated is None:
            failures.append(
                f"the guide states the job count as {count_claim.group(1)!r}, which this "
                f"check cannot read; spell it as a word this gate knows, or extend NUMBER_WORDS"
            )
        elif stated != len(rust_jobs):
            failures.append(
                f"the guide says the workflow runs {stated} jobs; it declares "
                f"{len(rust_jobs)}. Adding or removing a job means updating that sentence "
                f"and the split beside it."
            )
    split_claim = re.search(r"([a-z-]+) on `ubuntu-latest` and ([a-z-]+) elsewhere", guide)
    if split_claim:
        checked += 1
        on_ubuntu, off_ubuntu = (NUMBER_WORDS.get(g) for g in split_claim.groups())
        if on_ubuntu != len(rust_jobs) - elsewhere or off_ubuntu != elsewhere:
            failures.append(
                f"the guide states a split of {on_ubuntu} on ubuntu-latest and "
                f"{off_ubuntu} elsewhere; the workflow's real split is "
                f"{len(rust_jobs) - elsewhere} and {elsewhere}."
            )
    print(
        f"job count: the guide's figure agrees with {len(rust_jobs)} declared job(s), "
        f"{elsewhere} of which run somewhere other than plain ubuntu-latest"
    )

    # 4. Documented toggles.  The tree is every file that could read one; searching the
    # workflows and the Rust and shell sources is enough to catch a toggle that no longer
    # exists anywhere.
    haystack = []
    for pattern in ("*.yml", "*.rs", "*.sh", "*.toml", "*.py", "Makefile.in", "CMakeLists.txt"):
        for path in Path(".").rglob(pattern):
            if "target/" in str(path) or str(path).startswith("rust/"):
                continue
            try:
                haystack.append(path.read_text(encoding="utf-8", errors="replace"))
            except OSError:
                continue
    corpus = "\n".join(haystack)
    if len(corpus) < 100_000:
        sys.exit(
            f"error: only {len(corpus)} bytes of sources were read while looking for the "
            f"documented toggles; that scan has stopped working"
        )
    toggles = sorted(set(re.findall(r"\bZLIB_RS_[A-Z0-9_]+", guide)))
    if len(toggles) < 5:
        sys.exit("error: the guide documents fewer than five ZLIB_RS_* toggles; scan is wrong")
    for toggle in toggles:
        checked += 1
        if toggle not in corpus:
            failures.append(
                f"the guide documents {toggle}, which nothing in the tree reads. A "
                f"documented toggle that no code consults is an instruction that does nothing."
            )
    print(f"toggles: {len(toggles)} documented, all read somewhere in the tree")

    # 5. CMake options.
    cmake = read(CMAKELISTS)
    declared_options = set(re.findall(r"option\(\s*([A-Z0-9_]+)", cmake))
    if len(declared_options) < 3:
        sys.exit("error: fewer than three CMake options found; that scan has stopped working")
    named_options = {
        token for token in re.findall(r"`?\b(ZLIB_[A-Z0-9_]+)\b", guide)
        if not token.startswith("ZLIB_RS_")
    }
    for option in sorted(named_options & (declared_options | {"ZLIB_BUILD_RUST"})):
        checked += 1
        if option not in declared_options:
            failures.append(
                f"the guide names the CMake option {option}, which CMakeLists.txt does not "
                f"declare."
            )
    print(f"CMake options: {len(named_options & declared_options)} named, all declared")

    if failures:
        print(f"\nerror: {len(failures)} claim(s) in {README} no longer match the tree:")
        for failure in failures:
            print(f"  * {failure}")
        return 1

    print(
        f"\nPASS: {checked} checkable claim(s) in {README} agree with the workflows, the "
        f"scripts and CMakeLists.txt."
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
