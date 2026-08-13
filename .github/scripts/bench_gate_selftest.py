#!/usr/bin/env python3
"""Prove that ``bench_gate.py`` still enforces what it claims to enforce.

WHY THIS EXISTS
---------------
The benchmark gate spent its whole life unable to run.  It was written into a
temporary file by a ``run:`` step, it was three generations of parser spliced
together, and the only check the workflow made of it was
``python3 -m py_compile`` -- which validates syntax and nothing else.  So the
setup step passed, the gate raised ``NameError`` on its first real invocation,
and the AAP 0.8.4 limits went unenforced with the job reporting success.

A gate is a program, and an unexercised program is an assumption.  This runs the
real gate against a committed synthetic log -- ``bench_gate_fixtures/minimal_pass.log``,
a faithful sample of what ``cargo bench`` prints, whose authoritative cases have
been adjusted to sit inside the limits -- and then against a set of deliberately
broken variants of it.  The pass case must pass; every broken variant must fail,
and must fail with a message that names the thing that is wrong.  A gate that has
stopped enforcing something fails a case here, on every push, before it is ever
asked about a real measurement.

HOW THE CRITERION HALF IS EXERCISED WITHOUT COMMITTING CRITERION OUTPUT
----------------------------------------------------------------------
The gate's decision comes from ``<root>/<group>/<label>/<case>/new/estimates.json``,
not from the log.  Committing three dozen of those files would be noise, so this
script *synthesizes* them from the fixture's own ``port_ns`` and ``oracle_ns``
numbers into a temporary directory and points the gate at it with
``--criterion-root``.  The estimates therefore agree with the log by construction
in the pass case, and a variant can perturb one estimate on its own -- which is
how the "criterion decides, the self-timed line does not" property gets a test of
its own rather than a comment.

Usage::

    bench_gate_selftest.py            # runs every case, prints a verdict per case

Exit status is 0 when every case behaved as required and 1 otherwise.
"""

from __future__ import annotations

import json
import os
import re
import shutil
import subprocess
import sys
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
GATE = os.path.join(HERE, "bench_gate.py")
FIXTURE = os.path.join(HERE, "bench_gate_fixtures", "minimal_pass.log")

#: The suites whose authoritative groups the gate decides from criterion.  Only these
#: need synthesized estimates; everything else in the fixture is decided from the log.
#: The two tier-2 groups are here because they are authoritative too -- they are merely
#: exempt from the non-empty requirement while the corpus is not demanded, and the armed
#: case below withdraws that exemption.
CRITERION_GROUPS = (
    "deflate_steady_state",
    "inflate_steady_state",
    "deflate_silesia",
    "inflate_silesia",
)

RATIO = re.compile(
    r"^(?P<suite>\w+_bench): RATIO (?P<rest>group=\S+ .*)$"
)


def fields(text: str) -> dict[str, str]:
    """``key=value`` pairs from a gate line's tail."""
    return dict(part.split("=", 1) for part in text.split() if "=" in part)


def synthesize_estimates(log_lines: list[str], root: str) -> int:
    """Write one ``estimates.json`` per side per gated case, from the log's own numbers.

    Returns how many files were written, so a caller can assert the tree is not empty --
    an empty tree would make every criterion check fail for the wrong reason.
    """
    written = 0
    for line in log_lines:
        match = RATIO.match(line)
        if not match:
            continue
        field = fields(match.group("rest"))
        if field.get("group") not in CRITERION_GROUPS:
            continue
        if field.get("gate") != "counted":
            continue
        for label, key in (("zlib-rs", "port_ns"), ("c-oracle", "oracle_ns")):
            directory = os.path.join(root, field["group"], label, field["case"], "new")
            os.makedirs(directory, exist_ok=True)
            point = float(field[key])
            payload = {
                "mean": {"point_estimate": point},
                "median": {"point_estimate": point},
                "median_abs_dev": {"point_estimate": point / 1000.0},
                "slope": {"point_estimate": point},
                "std_dev": {"point_estimate": point / 1000.0},
            }
            with open(os.path.join(directory, "estimates.json"), "w", encoding="utf-8") as handle:
                json.dump(payload, handle)
            written += 1
    return written


def run_gate(log: str, root: str | None) -> tuple[int, str]:
    """Run the gate, returning ``(exit status, combined output)``."""
    command = [sys.executable, GATE, log]
    if root is not None:
        command += ["--criterion-root", root]
    finished = subprocess.run(
        command, capture_output=True, text=True, check=False
    )
    return finished.returncode, finished.stdout + finished.stderr


# =====================================================================================
#  The mutations.  Each returns the modified log text, and each names what it breaks.
# =====================================================================================


def drop_line(text: str, needle: str) -> str:
    """Every line containing `needle` removed."""
    kept = [line for line in text.split("\n") if needle not in line]
    assert len(kept) != len(text.split("\n")), f"the fixture carries no line matching {needle!r}"
    return "\n".join(kept)


def substitute(text: str, needle: str, old: str, new: str) -> str:
    """`old` replaced by `new`, but only on the lines containing `needle`."""
    out = []
    hits = 0
    for line in text.split("\n"):
        if needle in line and old in line:
            line = line.replace(old, new)
            hits += 1
        out.append(line)
    assert hits, f"the fixture carries no line matching {needle!r} with {old!r} in it"
    return "\n".join(out)


CASES: list[tuple[str, str]] = []


def case(name: str, why: str):
    """Register a failing case, and record what its failure has to be about."""

    def register(function):
        CASES.append((name, why))
        FAILING[name] = function
        return function

    return register


FAILING: dict[str, object] = {}


@case("missing authoritative summary", "no RATIO-SUMMARY line for group deflate_steady_state")
def _missing_summary(text: str) -> str:
    return drop_line(text, "RATIO-SUMMARY group=deflate_steady_state")


@case("a case that stopped being measured", "cases=14 but expected=15")
def _case_dropped(text: str) -> str:
    return substitute(
        text, "RATIO-SUMMARY group=deflate_steady_state", "cases=15", "cases=14"
    )


@case("a limit raised above the AAP's", "fixes the throughput limit at 1.10")
def _limit_raised(text: str) -> str:
    return substitute(
        text, "RATIO-SUMMARY group=inflate_steady_state", "limit=1.10", "limit=1.50"
    )


@case("an inventory limit raised above the AAP's", "AAP 0.8.4\nfixes it at 1.10")
def _inventory_limit(text: str) -> str:
    return substitute(text, "GATE-INVENTORY suite=deflate", "ratio_limit=1.10", "ratio_limit=1.40")


@case("the memory limit raised above the AAP's", "fixes the memory limit at 1.15")
def _memory_limit(text: str) -> str:
    return substitute(
        text, "MEMORY-SUMMARY group=deflate_memory", "limit=1.15", "limit=1.90"
    )


@case("a memory overrun", "exceeded the memory limit")
def _memory_over(text: str) -> str:
    return substitute(text, "MEMORY-SUMMARY group=deflate_memory", "over=0", "over=1")


@case("a dirty allocator tally", "reported a dirty allocator balance")
def _memory_dirty(text: str) -> str:
    return substitute(text, "MEMORY-SUMMARY group=deflate_memory", "dirty=0", "dirty=2")


@case("a per-stream footprint over budget", "exceeds the AAP 0.8.4 budget")
def _memory_bytes(text: str) -> str:
    return substitute(
        text,
        "MEMORY group=deflate_memory case=window_boundary.bin-L1-mem1",
        "port_bytes=138432",
        "port_bytes=999999",
    )


@case(
    "an overlap row that cost the caller an allocation",
    "must add nothing to the caller's high-water mark",
)
def _overlap_allocated(text: str) -> str:
    """The defect the pairing gate exists for: a caller-sized snapshot, reintroduced.

    `mem1` is the smallest state in the matrix and the fixture is 65 KiB, so an input-sized
    snapshot would roughly halve into the budget on this row -- 138432 + 66560 against a
    reference of 138064 is a ratio of 1.48 and would be caught by the ratio check too. The
    substitution here is deliberately far smaller than that, 1 KiB, so that the ratio stays
    at 1.011 and *only* the equality check can see it. That is the whole point: an allocation
    small enough to pass a percentage gate is still an allocation, and it is still on a path
    whose size the caller chooses.
    """
    return substitute(
        text,
        "MEMORY group=deflate_memory case=window_boundary.bin-L1-mem1-overlap",
        "port_bytes=138432",
        "port_bytes=139456",
    )


@case("an overlap row with no disjoint partner", "no disjoint row")
def _overlap_orphaned(text: str) -> str:
    """A row whose baseline is gone cannot show what the overlap added."""
    return drop_line(
        text, "MEMORY group=deflate_memory case=window_boundary.bin-L6-mem8 port_bytes"
    )


@case(
    "a configuration measured only with disjoint buffers",
    "row, so\nthis configuration",
)
def _overlap_missing_partner(text: str) -> str:
    """One overlap row dropped. Every other check still passes, which is the point."""
    return drop_line(text, "MEMORY group=deflate_memory case=window_boundary.bin-L9-mem9-overlap")


@case(
    "an overlap balance published under its disjoint partner's name",
    "no ALLOC-BALANCE line for side",
)
def _overlap_balance_misfiled(text: str) -> str:
    """The defect this actually caught in `deflate_bench.rs`, reproduced as a fixture.

    The suite formatted the `-overlap` suffix inline for the `MEMORY` row but handed the
    *unsuffixed* case id to its balance reporter, so every overlap case's two `ALLOC-BALANCE`
    rows were filed under the disjoint case's name. The visible result is what this fixture
    reproduces: the base case gets four balance rows instead of two, the overlap case gets
    none, and the row *count* is unchanged -- 36 lines either way -- which is why counting
    them is not the check. Two independent conditions have to fire: the duplicate-key check,
    because one side/case pair appeared twice, and the per-case presence check, because a
    memory case with no balance at all has an unmeasured allocator rather than a clean one.

    Distinct from `_overlap_retired`: there the measurement is gone, here it ran and its
    evidence was misattributed, which is the harder of the two to notice by reading a log.
    """
    lines = []
    for line in text.split("\n"):
        if "ALLOC-BALANCE" in line and "-overlap" in line:
            line = line.replace("-overlap ", " ")
        lines.append(line)
    changed = "\n".join(lines)
    assert changed != text, "the fixture carries no overlap ALLOC-BALANCE line"
    return changed


@case("the overlap measurement retired wholesale", "the bounded overlap stage was never measured")
def _overlap_retired(text: str) -> str:
    """Every overlap row dropped at once.

    Checked separately from the single-row case because it is the failure a per-row loop
    cannot see: with no overlap rows at all there is nothing to iterate over, and a gate that
    only compared the rows it found would report a clean pass over a measurement that had
    been deleted.
    """
    return drop_line(text, "-overlap")


# -------------------------------------------------------------------------------------
#  The optional `simd` feature's acceptance -- checksum_bench's BACKEND lines
# -------------------------------------------------------------------------------------


@case("a candidate backend slower than the path it displaces", "was SLOWER than")
def _backend_regressed(text: str) -> str:
    """The condition the whole acceptance sweep exists for.

    An optional backend that loses to its own baseline at some length is a bet on the
    caller's input size. The summary's own tally is moved here; `_backend_case_over`
    below moves a per-case time instead, so both the tally and the recomputation are
    covered and neither can be the only thing standing between a regression and a pass.
    """
    return substitute(text, "BACKEND-SUMMARY family=crc32", "over=0", "over=1")


@case("a per-case time that lost to its baseline", "the candidate took")
def _backend_case_over(text: str) -> str:
    """One case's candidate time raised past its own published tolerance.

    The summary still says `over=0`, so this is caught only by the gate recomputing the
    ratio from the two times -- which is the point of recomputing it.
    """
    return substitute(
        text,
        "BACKEND family=crc32 case=1048576",
        "candidate_ns=253699.5",
        "candidate_ns=999999.9",
    )


@case("a candidate that never beat its baseline anywhere", "improvement bar at any length")
def _backend_no_benefit(text: str) -> str:
    """A backend that merely matches its baseline.

    ★ This is the case the "never slower" condition alone would certify. `best=0.99` passes
    every per-case check -- nothing is slower than anything -- and still describes a second
    code path, behind a feature flag, that buys nothing. A feature has to earn the
    compile-time cost it adds.
    """
    return substitute(text, "BACKEND-SUMMARY family=adler32", "best=0.355", "best=0.990")


@case("an acceptance limit raised above 1.00", "the acceptance limit is 1.00")
def _backend_limit_raised(text: str) -> str:
    return substitute(text, "BACKEND-SUMMARY family=crc32", "limit=1.00", "limit=1.50")


@case("a benefit bar lowered below the AAP's 10%", "but the bar is 0.90")
def _backend_benefit_lowered(text: str) -> str:
    return substitute(text, "BACKEND-SUMMARY family=adler32", "benefit=0.90", "benefit=0.99")


@case("a case tolerance widened past the ceiling", "exceeds the ceiling of 0.10")
def _backend_tolerance_widened(text: str) -> str:
    """The loophole a self-measured tolerance would otherwise open.

    The suite derives each case's allowance from its own null measurement, which is better
    evidence than a constant -- but a suite that published a large one could pass anything,
    so the gate refuses any allowance above the same ceiling the suite itself applies.
    """
    return substitute(
        text,
        "BACKEND family=adler32 case=1048576",
        "tolerance=0.0137",
        "tolerance=0.5000",
    )


@case("a length that vanished without being reported", "was skipped for a reason")
def _backend_case_vanished(text: str) -> str:
    """`cases + unmeasured == expected` is an exact identity, and this breaks it.

    A length may produce a ratio or be refused as unmeasurable, and either way it is
    counted. One that disappears for a third reason -- a backend that disagreed with the C
    oracle, which the suite reports separately and never times -- shows up only here.
    """
    return substitute(text, "BACKEND-SUMMARY family=adler32", "cases=8", "cases=7")


@case("a run too noisy to conclude anything", "below the floor of 4")
def _backend_inconclusive(text: str) -> str:
    """`over=0` from a machine that could measure almost nothing is not a pass.

    Two cases measured and seven refused is an inconclusive run. Without the floor the
    gate would read its `over=0` as a clean bill of health for a comparison that never
    happened.
    """
    text = substitute(
        text, "BACKEND-SUMMARY family=crc32", "cases=6 over=0 unmeasured=3", "cases=2 over=0 unmeasured=7"
    )
    return drop_line(text, "BACKEND family=crc32 case=1")


@case("a backend family that stopped reporting", "no BACKEND-SUMMARY line for family")
def _backend_family_retired(text: str) -> str:
    """The failure a loop over the families present cannot see."""
    return drop_line(text, "BACKEND-SUMMARY family=adler32")


@case("a truncated log", "the log is incomplete")
def _truncated(text: str) -> str:
    return drop_line(text, "RATIO group=deflate_steady_state case=random.bin-L9")


@case("an unclean ledger row", "allocator balance for side=")
def _dirty_balance(text: str) -> str:
    return substitute(
        text,
        "ALLOC-BALANCE side=c-oracle case=window_boundary.bin-L1-mem1",
        "verdict=clean",
        "verdict=dirty",
    )


@case("a missing ledger side", "has no ALLOC-BALANCE line for side(s)")
def _missing_balance(text: str) -> str:
    return drop_line(text, "ALLOC-BALANCE side=zlib-rs case=window_boundary.bin-L1-mem8")


@case("a repeated ledger row", "the allocator ledger repeated")
def _repeated_balance(text: str) -> str:
    lines = text.split("\n")
    needle = "ALLOC-BALANCE side=zlib-rs case=window_boundary.bin-L1-mem1"
    index = next(i for i, line in enumerate(lines) if needle in line)
    lines.insert(index + 1, lines[index])
    return "\n".join(lines)


@case("an authoritative group moved out of the gate", "authoritative ratio groups are")
def _inventory_drift(text: str) -> str:
    return substitute(
        text,
        "GATE-INVENTORY suite=inflate",
        "authoritative_ratio=inflate_steady_state,inflate_silesia",
        "authoritative_ratio=inflate_silesia",
    )


@case("a summary contradicting its declared tier", "the inventory declares it")
def _tier_mismatch(text: str) -> str:
    return substitute(
        text,
        "RATIO-SUMMARY group=deflate_steady_state",
        "gate=authoritative",
        "gate=supporting",
    )


@case("levels 1, 6 and 9 not all gated", "no gated case was decided at compression level(s)")
def _missing_level(text: str) -> str:
    """The level requirement, on the suite whose gate quantity HAS a level.

    Deliberately ``deflate``: AAP 0.8.4's three levels qualify compression throughput, so
    that is where the requirement belongs and where it is still fatal.  The companion
    positive case in :func:`main` proves the same requirement is not applied to
    ``inflate_silesia``, which measures one level by construction.
    """
    out = []
    for line in text.split("\n"):
        if "RATIO group=deflate_steady_state" in line and "-L9 " in line:
            line = line.replace("gate=counted", "gate=informational")
        out.append(line)
    return "\n".join(out)


@case("a self-timed pass that was not order-alternating", "This ratio\nwas not produced by a paired")
def _fixed_order(text: str) -> str:
    return substitute(
        text,
        "RATIO group=deflate_steady_state case=random.bin-L6",
        "order=alternating",
        "order=port-first",
    )


@case("too few paired rounds", "below the 9 paired\nrounds")
def _too_few_rounds(text: str) -> str:
    return substitute(
        text,
        "RATIO group=inflate_steady_state case=random.bin-L6",
        "rounds=9",
        "rounds=3",
    )


@case("a ratio that is not its own quotient", "is not port_ns/oracle_ns")
def _inconsistent_ratio(text: str) -> str:
    return substitute(
        text,
        "RATIO group=deflate_steady_state case=window_boundary.bin-L6",
        "ratio=0.950",
        "ratio=0.500",
    )


@case("criterion rows registered in a fixed order", "registration='port-first'")
def _fixed_registration(text: str) -> str:
    return substitute(
        text,
        "CRITERION root=",
        "registration=alternating",
        "registration=port-first",
    )


@case("a required corpus that did not load", "the Silesia corpus was required but reports")
def _silesia_incomplete(text: str) -> str:
    return substitute(text, "SILESIA verdict=absent", "required=no", "required=yes")


@case("a duplicated summary", "exactly\none is expected")
def _duplicate_summary(text: str) -> str:
    lines = text.split("\n")
    needle = "RATIO-SUMMARY group=deflate_steady_state"
    index = next(i for i, line in enumerate(lines) if needle in line)
    lines.insert(index + 1, lines[index])
    return "\n".join(lines)


@case("a malformed line", "not a sequence of unique")
def _malformed(text: str) -> str:
    return substitute(
        text,
        "RATIO-SUMMARY group=inflate_steady_state",
        "informational=6",
        "informational",
    )


@case("a suite that never ran", "printed no gate line at all")
def _suite_absent(text: str) -> str:
    return "\n".join(
        line for line in text.split("\n") if not line.startswith("inflate_bench:")
    )


@case("an unknown suite emitting gate lines", "unrecognised bench suite")
def _unknown_suite(text: str) -> str:
    lines = text.split("\n")
    donor = next(line for line in lines if line.startswith("deflate_bench: RATIO-SUMMARY"))
    lines.append(donor.replace("deflate_bench:", "brotli_bench:", 1))
    return "\n".join(lines)


def arm_tier_two(text: str) -> str:
    """The same log as a completed acceptance run: corpus demanded and measured.

    This is the shape the ``bench`` job's armed steps produce, and building it here is
    how the armed path gets a test that needs no corpus.  Three cases per suite at the
    three gate levels, each large enough to be ``gate=counted``, each comfortably
    inside the limit; the summaries are rewritten to match, and the SILESIA line is
    rewritten to the complete, demanded verdict that withdraws the tier-2 exemption.
    """
    out: list[str] = []
    for line in text.split("\n"):
        if ": SILESIA verdict=" in line:
            suite = line.split(":", 1)[0]
            line = (
                f"{suite}: SILESIA verdict=complete found=12 of=12 required=yes "
                f"dir=/synthetic/target/silesia"
            )
        elif ": RATIO-SUMMARY group=" in line and "_silesia " in line:
            suite = line.split(":", 1)[0]
            group = f"{suite.split('_')[0]}_silesia"
            # ★ EACH SUITE'S REAL SCHEMA, not one shape for both, because the two
            # differ and the difference used to make an armed run unpassable.
            # `deflate_silesia` sweeps the three gate levels over a member;
            # `inflate_silesia` decodes each member at the ONE default level, so its
            # ids are `<member>-L6` and there is no L1 or L9 for a gate to demand.
            # An armed run of this exact shape must PASS.
            if group.startswith("deflate"):
                cases = [
                    ("dickens-L1", 10192446, 4200000.0),
                    ("dickens-L6", 10192446, 9100000.0),
                    ("dickens-L9", 10192446, 21000000.0),
                ]
            else:
                cases = [
                    ("dickens-L6", 10192446, 31000000.0),
                    ("mozilla-L6", 51220480, 152000000.0),
                    ("webster-L6", 41458703, 121000000.0),
                ]
            out.append(
                f"{suite}: RATIO-SUMMARY group={group} gate=authoritative expected=3 "
                f"cases=3 gated=3 over=0 informational=0 limit=1.10"
            )
            for case_id, size, oracle in cases:
                out.append(
                    f"{suite}: RATIO group={group} case={case_id} bytes={size} "
                    f"port_ns={oracle * 0.94:.1f} oracle_ns={oracle:.1f} ratio=0.940 "
                    f"ratio_hi=1.053 rounds=9 order=alternating "
                    f"limit=1.10 gate=counted verdict=within"
                )
            continue
        out.append(line)
    return "\n".join(out)


def main() -> int:
    """Run the pass case and every failing case.  See the module docstring."""
    with open(FIXTURE, encoding="utf-8") as handle:
        fixture = handle.read()
    fixture_lines = fixture.split("\n")

    workspace = tempfile.mkdtemp(prefix="bench_gate_selftest_")
    outcomes: list[tuple[bool, str, str]] = []
    try:
        root = os.path.join(workspace, "criterion")
        written = synthesize_estimates(fixture_lines, root)
        if written == 0:
            print("::error::the fixture yielded no gated case, so nothing would be decided")
            return 1

        # ---- The pass case ------------------------------------------------------
        good = os.path.join(workspace, "pass.log")
        with open(good, "w", encoding="utf-8") as handle:
            handle.write(fixture)
        status, output = run_gate(good, root)
        ok = status == 0 and "benchmark regression: PASS" in output
        outcomes.append((ok, "the committed pass fixture", "" if ok else output))

        # The pass case must ALSO have decided something, or "PASS" means "measured
        # nothing".  The count the gate prints is the assertion.
        decided = re.search(r"criterion decisions: (\d+) gated case", output)
        enough = decided is not None and int(decided.group(1)) >= 18
        outcomes.append(
            (
                enough,
                "the pass fixture decides at least 18 gated cases from criterion",
                "" if enough else output,
            )
        )

        # A warning must not be fatal: the fixture keeps its supporting-group
        # overruns, which the gate reports and does not enforce.
        warned = "::warning::" in output
        outcomes.append(
            (
                warned,
                "a supporting-group overrun warns without failing",
                "" if warned else output,
            )
        )

        # ---- The armed acceptance shape, which needs no corpus to test ----------
        # The `bench` job's last three steps only run when tier-2 acceptance is armed,
        # so on an unarmed run they are never exercised at all.  This is that path: the
        # corpus reports complete and demanded, the tier-2 exemption is withdrawn, and
        # the gate must decide the silesia cases from criterion like any other
        # authoritative group.
        armed_text = arm_tier_two(fixture)
        armed = os.path.join(workspace, "armed.log")
        with open(armed, "w", encoding="utf-8") as handle:
            handle.write(armed_text)
        armed_root = os.path.join(workspace, "criterion-armed")
        synthesize_estimates(armed_text.split("\n"), armed_root)
        status, output = run_gate(armed, armed_root)
        decided_armed = re.search(r"criterion decisions: (\d+) gated case", output)
        ok = (
            status == 0
            and "benchmark regression: PASS" in output
            and decided_armed is not None
            and int(decided_armed.group(1)) >= 24
        )
        outcomes.append(
            (ok, "an armed tier-2 acceptance run", "" if ok else output)
        )

        # And armed acceptance must refuse a tier-2 group that measured nothing, which
        # is the exact degradation AAP 0.6.4.4's exemption could otherwise hide.
        hollow = os.path.join(workspace, "armed-hollow.log")
        with open(hollow, "w", encoding="utf-8") as handle:
            handle.write(
                substitute(
                    armed_text,
                    "RATIO-SUMMARY group=deflate_silesia",
                    "expected=3 cases=3 gated=3",
                    "expected=0 cases=0 gated=0",
                )
            )
        status, output = run_gate(hollow, armed_root)
        ok = status == 1 and "expected=0" in output
        outcomes.append(
            (ok, "an armed run whose tier-2 group measured nothing", "" if ok else output)
        )

        # ---- A diagnostic overrun that criterion contradicts --------------------
        # The property this proves is the authority model the CRITERION line declares:
        # `self_timed=diagnostic authority=criterion`.  An authoritative group whose
        # self-timed pass reports cases over the limit -- which a bounded nine-round
        # probe on a loaded machine does routinely -- must WARN and let criterion decide,
        # and criterion here says every case is inside the limit.  Before this, the
        # diagnostic failed the build before criterion was consulted at all.
        diagnostic = os.path.join(workspace, "diagnostic-over.log")
        over_text = substitute(
            substitute(
                fixture,
                "RATIO-SUMMARY group=deflate_steady_state",
                "over=0",
                "over=2",
            ),
            "RATIO group=deflate_steady_state case=random.bin-L6",
            "verdict=within",
            "verdict=over",
        )
        with open(diagnostic, "w", encoding="utf-8") as handle:
            handle.write(over_text)
        status, output = run_gate(diagnostic, root)
        ok = (
            status == 0
            and "benchmark regression: PASS" in output
            and "self-timed diagnostic" in output
            and "::error::" not in output
        )
        outcomes.append(
            (
                ok,
                "a diagnostic overrun warns while criterion passes the run",
                "" if ok else output,
            )
        )

        # ---- A perturbed criterion estimate, with the log left alone ------------
        # The property this proves: the DECISION comes from criterion, so a log whose
        # own self-timed numbers are all within the limit still fails when the
        # estimates say otherwise.
        perturbed = os.path.join(workspace, "criterion-perturbed")
        shutil.copytree(root, perturbed)
        target = os.path.join(
            perturbed, "deflate_steady_state", "zlib-rs", "random.bin-L6", "new", "estimates.json"
        )
        with open(target, encoding="utf-8") as handle:
            payload = json.load(handle)
        for key in ("mean", "slope"):
            payload[key]["point_estimate"] *= 4.0
        with open(target, "w", encoding="utf-8") as handle:
            json.dump(payload, handle)
        status, output = run_gate(good, perturbed)
        ok = status == 1 and "exceeds the AAP 0.8.4 limit" in output
        outcomes.append(
            (ok, "criterion decides, not the self-timed line", "" if ok else output)
        )

        # ---- A gated case criterion never measured -----------------------------
        stripped = os.path.join(workspace, "criterion-stripped")
        shutil.copytree(root, stripped)
        shutil.rmtree(os.path.join(stripped, "inflate_steady_state", "c-oracle", "random.bin-L1"))
        status, output = run_gate(good, stripped)
        ok = status == 1 and "criterion produced no usable estimate" in output
        outcomes.append(
            (ok, "a gated case with no criterion estimate", "" if ok else output)
        )

        # ---- A criterion root that is not there --------------------------------
        status, output = run_gate(good, os.path.join(workspace, "absent"))
        ok = status == 1 and "which is not a directory" in output
        outcomes.append((ok, "a criterion root that does not exist", "" if ok else output))

        # ---- An empty log ------------------------------------------------------
        empty = os.path.join(workspace, "empty.log")
        with open(empty, "w", encoding="utf-8") as handle:
            handle.write("cargo bench produced nothing useful\n")
        status, output = run_gate(empty, root)
        ok = status == 1 and "carries no bench gate lines at all" in output
        outcomes.append((ok, "a log with no gate lines", "" if ok else output))

        # ---- A log that is not there at all ------------------------------------
        status, output = run_gate(os.path.join(workspace, "nope.log"), root)
        ok = status == 1 and "could not be read" in output
        outcomes.append((ok, "a log that does not exist", "" if ok else output))

        # ---- Every registered mutation -----------------------------------------
        for name, why in CASES:
            mutated = FAILING[name](fixture)
            path = os.path.join(workspace, re.sub(r"\W+", "_", name) + ".log")
            with open(path, "w", encoding="utf-8") as handle:
                handle.write(mutated)
            status, output = run_gate(path, root)
            flattened = " ".join(output.split())
            wanted = " ".join(why.split())
            ok = status == 1 and wanted in flattened
            outcomes.append((ok, name, "" if ok else f"wanted {wanted!r} in:\n{output}"))
    finally:
        shutil.rmtree(workspace, ignore_errors=True)

    failed = [(name, detail) for ok, name, detail in outcomes if not ok]
    for ok, name, _ in outcomes:
        print(f"  {'PASS' if ok else 'FAIL'}  {name}")
    if failed:
        for name, detail in failed:
            print(f"::error::bench gate self-test case {name!r} did not behave as required")
            print(detail)
        print(f"bench gate self-test: FAIL -- {len(failed)} of {len(outcomes)} case(s)")
        return 1
    print(f"bench gate self-test: PASS -- {len(outcomes)} case(s)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
