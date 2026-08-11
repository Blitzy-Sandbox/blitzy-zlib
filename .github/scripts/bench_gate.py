#!/usr/bin/env python3
"""Enforce the AAP performance gates over one benchmark log.

WHY THIS IS A FILE IN THE REPOSITORY AND NOT A HEREDOC IN THE WORKFLOW
----------------------------------------------------------------------
It used to be written into ``$RUNNER_TEMP`` by a ``run:`` step, and the copy that
was written there was three different generations of parser spliced together: it
defined ``number()`` twice and referenced sixty-odd names -- ``LINE``, ``KEYS``,
``EXPECTED``, ``fail``, ``summaries``, ``cases``, ``failures``, ``balances``,
``seen_summary``, ``csv`` -- that no statement in it ever bound.  It compiled,
because ``python3 -m py_compile`` only checks syntax, so the workflow's own
"a syntax error here must surface as a setup failure" guard passed; the first
real invocation then raised ``NameError`` and the 1.10 / 1.15 limits were never
applied to anything.

Being a committed file is the structural fix, not a tidy-up: a file can be
compiled, linted, diffed, reviewed, and -- decisively -- *tested*.
``bench_gate_selftest.py`` beside it runs this program against a committed
synthetic log and against a set of deliberately broken variants of it, and the
workflow runs that self-test before it runs the real gate.  A gate that cannot
fail its own self-test cannot silently stop enforcing anything again.

WHAT IT ENFORCES
----------------
``benches/deflate_bench.rs`` and ``benches/inflate_bench.rs`` print a
machine-readable contract to stderr and enforce nothing themselves -- their own
documentation says "Reported, never enforced -- the workflow decides".  This is
that decision.  The three numbered targets are AAP 0.8.4:

* decompression throughput within 10% of C zlib  -> ``inflate_steady_state``
* compression throughput within 10% at levels 1, 6 and 9 -> ``deflate_steady_state``
* per-stream memory within 15% -> ``deflate_memory``

Those are exactly the groups the suites declare ``gate=authoritative``, and they
are the only groups whose overrun FAILS this job.  A ``supporting`` group
measures "more or other than the gate's quantity", which its own source says
makes a regression there "a signal rather than a verdict", and an
``informational`` group is outside the gate by construction.  Both are reported
-- supporting overruns as ``::warning::`` -- and neither is fatal, because
failing on them would gate quantities the AAP does not name.

★ WHICH NUMBER DECIDES.  The ``RATIO`` lines the suites print are a self-timed
diagnostic: a short calibration loop, best-of, no outlier rejection.  The
measurement AAP 0.6.4.6 names is criterion's, and criterion writes it to disk.
So for every gated case of an authoritative throughput group this program reads
``<root>/<group>/<label>/<case>/new/estimates.json`` for both sides, prefers the
``slope`` point estimate over ``mean`` exactly as the ``CRITERION`` line
declares, and computes the ratio itself.  A missing estimates file for a gated
case is a failure: it means criterion did not measure a case the summary counted.
Memory is decided from the ``MEMORY`` lines' byte counts instead, because the
memory group deliberately registers no criterion row -- the allocation counter's
bookkeeping and its 0xa5 fill would contaminate a rate.

WHAT IT REFUSES TO READ AS SUCCESS
----------------------------------
Every check below exists because some green-looking log satisfies its absence:

* a log with no gate lines at all -- nothing was measured;
* a summary that is present but empty (``expected=0 cases=0``), which ``over=0``
  looks exactly like;
* an authoritative group whose summary is missing entirely;
* two summaries for one group, which means the log was concatenated;
* ``cases != expected``, which means a fixture or a level stopped being measured;
* a summary counting more cases than the log carries per-case lines for, which is
  a log truncated mid-group or a suite that died after printing its summary;
* a limit on the line that is not the AAP's number -- a gate that accepts
  whatever limit the bench prints enforces nothing;
* gated cases that never reach level 1, 6 or 9;
* an allocator ledger with a missing side, a repeated row or an unclean verdict;
* a suite emitting gate lines that this program does not know about, which would
  otherwise be measured and never enforced.

Usage::

    bench_gate.py <bench log>            # criterion root comes from the log
    bench_gate.py <bench log> --criterion-root <dir>   # override, for the self-test

Exit status is 0 when every check passed and 1 otherwise, with one
``::error::`` annotation per failure.
"""

from __future__ import annotations

import json
import os
import re
import sys

# =====================================================================================
#  The AAP's numbers.  One copy, here.
# =====================================================================================

#: AAP 0.8.4: throughput within 10% of the C reference.
RATIO_LIMIT = 1.10

#: AAP 0.8.4: per-stream memory within 15% of the C reference.
MEMORY_LIMIT = 1.15

#: AAP 0.8.4 names levels 1, 6 and 9 explicitly.  The suites publish the same axis
#: on their ``GATE-INVENTORY`` line as ``levels=1,6,9``, and a case id carries it as
#: an ``-L<n>`` element, so the two can be checked against each other.
REQUIRED_LEVELS = ("L1", "L6", "L9")

#: Below one page an encode is fixed cost rather than throughput, which is why the
#: suites mark smaller cases ``gate=informational``.  Checked, not assumed: a bench
#: that raised its own floor would drop cases out of the gate silently.
MIN_BYTES = "4096"

# =====================================================================================
#  What each suite must declare, and the floors its authoritative groups must clear.
# =====================================================================================
#
# The counts are FLOORS rather than equalities on purpose.  `cases == expected` below
# already catches a case that stopped being measured, and it catches it without this
# program having to know how many fixtures the suites carry -- so adding a fixture is
# a one-file change rather than two.  What a floor adds is the thing `cases ==
# expected` cannot see: a suite edited until it sets out to measure nothing at all
# would satisfy `cases == expected` perfectly at zero.  Nine is
# len(REQUIRED_LEVELS) x 3 gate-eligible fixtures, which is what the committed
# minimal corpus yields today; it can only be lowered deliberately.

SUITES = {
    "deflate_bench": {
        "suite": "deflate",
        "gates_memory": True,
        "authoritative_ratio": ("deflate_steady_state", "deflate_silesia"),
        "authoritative_memory": ("deflate_memory",),
        # The tier-2 group is authoritative but legitimately empty when the opt-in
        # corpus is absent, which AAP 0.6.4.4 requires CI to tolerate.  It stops
        # being exempt the moment the SILESIA line reports required=yes.
        "tier2_ratio": ("deflate_silesia",),
        "min_gated": {"deflate_steady_state": 9},
        "min_cases": {"deflate_memory": 9},
    },
    "inflate_bench": {
        "suite": "inflate",
        "gates_memory": False,
        "authoritative_ratio": ("inflate_steady_state", "inflate_silesia"),
        "authoritative_memory": (),
        "tier2_ratio": ("inflate_silesia",),
        "min_gated": {"inflate_steady_state": 9},
        "min_cases": {},
    },
}

#: ``checksum_bench`` prints no summary line of any kind and is absent from
#: :data:`SUITES` on purpose: a checksum produces one scalar however it is computed,
#: so there is no output-fidelity dimension to trade against speed and nothing to
#: gate.  Do not "fix" that by adding it -- but do not let it be parsed as a gate
#: suite either, which is why an unknown suite emitting gate lines is a failure.

#: One ledger line per side per memory case, and both sides are required: a balance
#: that is clean on the port and never measured on the oracle proves only that the
#: counter ran.
BALANCE_SIDES = ("zlib-rs", "c-oracle")

#: Keys each line kind must carry.  A line missing one is malformed, not tolerated:
#: reading a default in place of an absent key is how a gate ends up enforcing a
#: value the suite never printed.
REQUIRED_KEYS = {
    "GATE-INVENTORY": (
        "suite",
        "ratio_limit",
        "min_bytes",
        "levels",
        "authoritative_ratio",
        "supporting_ratio",
        "informational_ratio",
    ),
    "CRITERION": (
        "root",
        "port_label",
        "oracle_label",
        "layout",
        "estimate",
        "fallback",
        "authority",
        "self_timed",
    ),
    "RATIO": (
        "group",
        "case",
        "bytes",
        "port_ns",
        "oracle_ns",
        "ratio",
        "limit",
        "gate",
        "verdict",
    ),
    "RATIO-SUMMARY": (
        "group",
        "gate",
        "expected",
        "cases",
        "gated",
        "over",
        "informational",
        "limit",
    ),
    "MEMORY": ("group", "case", "port_bytes", "oracle_bytes", "ratio", "limit", "verdict"),
    "MEMORY-SUMMARY": ("group", "gate", "expected", "cases", "over", "dirty", "limit"),
    "ALLOC-BALANCE": (
        "side",
        "case",
        "allocations",
        "frees",
        "live_bytes",
        "notlifo",
        "rogue",
        "refusals",
        "verdict",
    ),
    "SILESIA": ("verdict", "found", "of", "required", "dir"),
}

#: The keyword must follow ``"<suite>: "`` IMMEDIATELY.  The suites also print their
#: own format documentation with the same prefix, and that prose contains the literal
#: text ``RATIO-SUMMARY`` inside backticks; anchoring on the keyword's position is
#: what keeps documentation from being parsed as data.  ``RATIO-SUMMARY`` is listed
#: before ``RATIO`` because the alternation is ordered.
LINE = re.compile(
    r"^(?P<suite>\w+_bench): "
    r"(?P<kind>GATE-INVENTORY|CRITERION|RATIO-SUMMARY|MEMORY-SUMMARY|RATIO|MEMORY"
    r"|ALLOC-BALANCE|SILESIA) "
    r"(?P<rest>\S+=\S*(?: .*)?)$"
)

#: ``<fixture>-L<level>`` with an optional ``-mem<n>`` tail, so the level a case
#: measures is readable from its id.
LEVEL = re.compile(r"-(?P<level>L\d+)(?:-mem\d+)?$")


class Report:
    """Accumulated failures, warnings and notes for one run of the gate."""

    def __init__(self) -> None:
        self.failures: list[str] = []
        self.warnings: list[str] = []
        self.notes: list[str] = []

    def fail(self, message: str) -> None:
        self.failures.append(message)

    def warn(self, message: str) -> None:
        self.warnings.append(message)

    def note(self, message: str) -> None:
        self.notes.append(message)


def pairs(text: str) -> dict[str, str] | None:
    """``key=value`` pairs, or ``None`` if the text is not exactly that.

    Duplicate keys are rejected rather than resolved: a line carrying ``over=0`` twice
    has one value the reader would use and one it would not, and no rule that picks
    between them is defensible.
    """
    out: dict[str, str] = {}
    for part in text.split():
        if "=" not in part:
            return None
        key, value = part.split("=", 1)
        if key in out:
            return None
        out[key] = value
    return out


def as_int(field: dict[str, str], key: str) -> int | None:
    """``field[key]`` as an integer, or ``None`` when it is absent or not a number."""
    try:
        return int(field[key])
    except (KeyError, ValueError):
        return None


def as_float(field: dict[str, str], key: str) -> float | None:
    """``field[key]`` as a float, or ``None`` when it is absent or not a number."""
    try:
        return float(field[key])
    except (KeyError, ValueError):
        return None


def csv(value: str) -> list[str]:
    """A comma-separated list, with the empty string meaning no members.

    The suites print the key even when no group carries the policy -- ``a consumer
    never has to distinguish "no such key" from "no such group"`` -- so ``""`` has to
    mean an empty list rather than one empty name.
    """
    return [item for item in value.split(",") if item]


def parse(path: str, report: Report) -> dict[str, dict[str, list[dict[str, str]]]]:
    """Read `path` into ``{suite: {kind: [fields, ...]}}``, reporting malformed lines."""
    try:
        with open(path, encoding="utf-8", errors="replace") as handle:
            lines = [line.rstrip("\n") for line in handle]
    except OSError as error:
        report.fail(f"the benchmark log {path} could not be read: {error}")
        return {}

    seen: dict[str, dict[str, list[dict[str, str]]]] = {}
    for line in lines:
        match = LINE.match(line)
        if not match:
            continue
        suite, kind = match.group("suite"), match.group("kind")
        field = pairs(match.group("rest"))
        if field is None:
            report.fail(
                f"{suite}: malformed {kind} line -- not a sequence of unique "
                f"key=value pairs: {line}"
            )
            continue
        missing = [key for key in REQUIRED_KEYS[kind] if key not in field]
        if missing:
            report.fail(
                f"{suite}: {kind} line is missing required key(s) "
                f"{', '.join(missing)}: {line}"
            )
            continue
        seen.setdefault(suite, {}).setdefault(kind, []).append(field)
    return seen


def one(
    suite: str,
    kind: str,
    found: list[dict[str, str]],
    why: str,
    report: Report,
) -> dict[str, str] | None:
    """The single line of `kind`, or ``None`` with a failure recorded."""
    if len(found) != 1:
        report.fail(
            f"{suite}: expected exactly 1 {kind} line, found {len(found)}. {why}"
        )
        return None
    return found[0]


def check_inventory(
    suite: str,
    spec: dict,
    inventory: dict[str, str],
    report: Report,
) -> dict[str, str]:
    """Check the suite's declaration against this gate's, and return the group tiers.

    The point of the ``GATE-INVENTORY`` line is that the two sides can be compared: a
    suite that moved a group out of its authoritative set, or a gate that is out of
    date about which groups carry the AAP's numbers, must stop the build rather than
    quietly measure and enforce different things.
    """
    if inventory["suite"] != spec["suite"]:
        report.fail(
            f"{suite}: inventory says suite={inventory['suite']} but this gate "
            f"expects {spec['suite']}"
        )

    declared_ratio_limit = as_float(inventory, "ratio_limit")
    if declared_ratio_limit is None or abs(declared_ratio_limit - RATIO_LIMIT) > 1e-9:
        report.fail(
            f"{suite}: inventory ratio_limit={inventory['ratio_limit']} but AAP 0.8.4 "
            f"fixes it at {RATIO_LIMIT:.2f}. A gate that accepts the limit the bench "
            f"prints enforces nothing"
        )

    if spec["gates_memory"]:
        declared_memory_limit = as_float(inventory, "memory_limit")
        if (
            declared_memory_limit is None
            or abs(declared_memory_limit - MEMORY_LIMIT) > 1e-9
        ):
            report.fail(
                f"{suite}: inventory memory_limit={inventory.get('memory_limit')} but "
                f"AAP 0.8.4 fixes it at {MEMORY_LIMIT:.2f}"
            )
    elif "memory_limit" in inventory:
        report.fail(
            f"{suite}: inventory declares memory_limit={inventory['memory_limit']} but "
            f"this suite is not expected to gate memory, so a memory verdict from it "
            f"would be unenforced"
        )

    if inventory["min_bytes"] != MIN_BYTES:
        report.fail(
            f"{suite}: inventory min_bytes={inventory['min_bytes']} but this gate "
            f"expects {MIN_BYTES}. Raising the floor removes cases from the gate"
        )

    levels = csv(inventory["levels"])
    want_levels = [level.lstrip("L") for level in REQUIRED_LEVELS]
    if levels != want_levels:
        report.fail(
            f"{suite}: inventory levels={inventory['levels']} but AAP 0.8.4 names "
            f"{','.join(want_levels)}"
        )

    tiers: dict[str, str] = {}
    for tier in ("authoritative", "supporting", "informational"):
        for group in csv(inventory[f"{tier}_ratio"]):
            if group in tiers:
                report.fail(
                    f"{suite}: group {group} is declared in more than one ratio tier, "
                    f"so its summary's standing is ambiguous"
                )
            tiers[group] = tier

    authoritative = sorted(g for g, t in tiers.items() if t == "authoritative")
    if authoritative != sorted(spec["authoritative_ratio"]):
        report.fail(
            f"{suite}: authoritative ratio groups are {authoritative} but this gate "
            f"enforces {sorted(spec['authoritative_ratio'])}. Either the bench moved a "
            f"group out of the gate or the gate is out of date; both need a human"
        )

    memory_groups = sorted(csv(inventory.get("authoritative_memory", "")))
    if memory_groups != sorted(spec["authoritative_memory"]):
        report.fail(
            f"{suite}: authoritative memory groups are {memory_groups} but this gate "
            f"enforces {sorted(spec['authoritative_memory'])}"
        )

    return tiers


def silesia_required(suite: str, got: dict, report: Report) -> bool:
    """Whether the tier-2 corpus was demanded, from the suite's own SILESIA line.

    AAP 0.6.4.4 makes the corpus opt-in, so ``required=no`` earns the tier-2 groups an
    exemption from "an authoritative group must have measured something".
    ``required=yes`` withdraws it, and then the corpus must also be the complete pinned
    twelve -- a number measured over eight of them is not the number AAP 0.8.4 is about.
    """
    line = one(
        suite,
        "SILESIA",
        got.get("SILESIA", []),
        "Without it the gate cannot tell an opt-in corpus that is absent by design "
        "from one that failed to load.",
        report,
    )
    if line is None:
        return False

    if line["required"] not in ("yes", "no"):
        report.fail(
            f"{suite}: SILESIA required={line['required']} is neither yes nor no"
        )
        return False
    if line["required"] == "no":
        report.note(
            f"{suite}: the opt-in Silesia corpus was not demanded "
            f"(verdict={line['verdict']} found={line['found']} of {line['of']}), so its "
            f"tier-2 group is exempt from the non-empty requirement -- AAP 0.6.4.4"
        )
        return False

    if line["verdict"] != "complete":
        report.fail(
            f"{suite}: the Silesia corpus was required but reports "
            f"verdict={line['verdict']} found={line['found']} of {line['of']}. AAP 0.8.4 "
            f"names the complete corpus, and a number measured over part of it is not "
            f"that number"
        )
    elif line["found"] != line["of"]:
        report.fail(
            f"{suite}: the Silesia corpus reports verdict=complete but found="
            f"{line['found']} of {line['of']}"
        )
    return True


def estimate_ns(root: str, group: str, label: str, case: str) -> tuple[float | None, str]:
    """criterion's point estimate in nanoseconds for one side of one case.

    ``slope`` in preference to ``mean``, which is what the suites' ``CRITERION`` line
    declares: criterion fits the slope across the whole sample and its standard error
    was measured at a third of the mean's on a loaded machine.  The second element is
    the path, so a failure can say which file was consulted.
    """
    path = os.path.join(root, group, label, case, "new", "estimates.json")
    try:
        with open(path, encoding="utf-8") as handle:
            payload = json.load(handle)
    except (OSError, ValueError):
        return None, path
    for key in ("slope", "mean"):
        entry = payload.get(key)
        if isinstance(entry, dict):
            value = entry.get("point_estimate")
            if isinstance(value, (int, float)):
                return float(value), path
    return None, path


def gate_ratio_group(
    suite: str,
    group: str,
    summary: dict[str, str],
    observed: list[dict[str, str]],
    criterion: dict[str, str],
    root: str,
    tier: str,
    exempt: bool,
    spec: dict,
    report: Report,
) -> list[tuple[str, float]]:
    """Check one throughput group, returning the criterion ratios it decided from."""
    expected = as_int(summary, "expected")
    cases = as_int(summary, "cases")
    gated = as_int(summary, "gated")
    over = as_int(summary, "over")
    limit = as_float(summary, "limit")

    if summary["gate"] != tier:
        report.fail(
            f"{suite} group {group}: the summary says gate={summary['gate']} but the "
            f"inventory declares it {tier}; one of the two is wrong and the group's "
            f"standing cannot be decided"
        )

    for name, value in (("expected", expected), ("cases", cases), ("gated", gated), ("over", over)):
        if value is None:
            report.fail(
                f"{suite} group {group}: {name}={summary.get(name)!r} is not a number, "
                f"so the summary line is malformed"
            )
            return []

    if limit is None or abs(limit - RATIO_LIMIT) > 1e-9:
        report.fail(
            f"{suite} group {group}: reports limit={summary.get('limit')} but AAP 0.8.4 "
            f"fixes the throughput limit at {RATIO_LIMIT:.2f}"
        )

    complain = report.fail if tier == "authoritative" else report.warn
    if cases != expected:
        complain(
            f"{suite} group {group}: cases={cases} but expected={expected}. The group "
            f"set out to measure {expected} case(s) and produced {cases}, so something "
            f"was skipped -- the usual cause is a fixture that did not load"
        )
    if len(observed) != cases:
        complain(
            f"{suite} group {group}: the summary counts cases={cases} but the log "
            f"carries {len(observed)} per-case RATIO line(s), so the log is incomplete"
        )

    ids = [field["case"] for field in observed]
    duplicates = sorted({name for name in ids if ids.count(name) > 1})
    if duplicates:
        complain(
            f"{suite} group {group}: duplicate case id(s) {', '.join(duplicates)}"
        )

    if tier == "authoritative" and not exempt:
        floor = spec["min_gated"].get(group, 1)
        if expected <= 0:
            report.fail(
                f"{suite} group {group}: expected=0. This group carries one of the AAP "
                f"0.8.4 targets and set out to measure nothing, which over=0 looks "
                f"exactly like"
            )
        if gated < floor:
            report.fail(
                f"{suite} group {group}: gated={gated}, below the floor of {floor}. "
                f"Every case is informational or absent, so nothing in the group can "
                f"be enforced"
            )

    if over:
        what = "throughput"
        message = (
            f"{suite}: {over} gated case(s) in group {group} exceeded the {what} limit "
            f"of {summary.get('limit')} on the suite's own self-timed diagnostic"
        )
        if tier == "authoritative":
            report.fail(message)
        else:
            report.warn(
                message + " -- reported, not fatal: the suite declares this group "
                f"gate={tier}, so a regression here is a signal rather than a verdict"
            )

    # ★ The decision, from criterion's estimates rather than from the self-timed
    # numbers above.  Only the gated cases of an authoritative group are decided:
    # an informational case carries a ratio but is outside the gate by size, and a
    # supporting group is outside it by policy.
    decided: list[tuple[str, float]] = []
    if tier != "authoritative" or exempt:
        return decided

    for field in observed:
        if field["gate"] != "counted":
            continue
        case = field["case"]
        port, port_path = estimate_ns(root, group, criterion["port_label"], case)
        oracle, oracle_path = estimate_ns(root, group, criterion["oracle_label"], case)
        if port is None or oracle is None:
            missing = port_path if port is None else oracle_path
            report.fail(
                f"{suite} group {group} case {case}: the summary counts it as gated but "
                f"criterion produced no usable estimate at {missing}. AAP 0.6.4.6's "
                f"measurement is criterion's, so a gated case without one has not been "
                f"measured"
            )
            continue
        if oracle <= 0.0:
            report.fail(
                f"{suite} group {group} case {case}: criterion reports the reference at "
                f"{oracle} ns, which cannot be a ratio's denominator"
            )
            continue
        ratio = port / oracle
        decided.append((case, ratio))
        if ratio > RATIO_LIMIT:
            report.fail(
                f"{suite} group {group} case {case}: criterion ratio "
                f"{ratio:.3f} exceeds the AAP 0.8.4 limit of {RATIO_LIMIT:.2f} "
                f"(port {port:.1f} ns vs reference {oracle:.1f} ns)"
            )

    if not decided:
        report.fail(
            f"{suite} group {group}: not one gated case was decided from criterion's "
            f"estimates, so the AAP 0.8.4 target this group carries was not enforced"
        )

    levels = {
        match.group("level")
        for match in (LEVEL.search(case) for case, _ in decided)
        if match
    }
    absent = [level for level in REQUIRED_LEVELS if level not in levels]
    if absent:
        report.fail(
            f"{suite} group {group}: no gated case was decided at compression level(s) "
            f"{', '.join(absent)}. AAP 0.8.4 names levels 1, 6 and 9 explicitly; the "
            f"levels decided were {', '.join(sorted(levels)) or 'none'}"
        )
    return decided


def gate_memory_group(
    suite: str,
    group: str,
    summary: dict[str, str],
    observed: list[dict[str, str]],
    spec: dict,
    report: Report,
) -> None:
    """Check the memory group, which criterion never times and bytes decide."""
    expected = as_int(summary, "expected")
    cases = as_int(summary, "cases")
    over = as_int(summary, "over")
    dirty = as_int(summary, "dirty")
    limit = as_float(summary, "limit")

    for name, value in (("expected", expected), ("cases", cases), ("over", over), ("dirty", dirty)):
        if value is None:
            report.fail(
                f"{suite} group {group}: {name}={summary.get(name)!r} is not a number"
            )
            return

    if limit is None or abs(limit - MEMORY_LIMIT) > 1e-9:
        report.fail(
            f"{suite} group {group}: reports limit={summary.get('limit')} but AAP 0.8.4 "
            f"fixes the memory limit at {MEMORY_LIMIT:.2f}"
        )
    if cases != expected:
        report.fail(
            f"{suite} group {group}: cases={cases} but expected={expected}, so a "
            f"configuration was skipped"
        )
    if len(observed) != cases:
        report.fail(
            f"{suite} group {group}: the summary counts cases={cases} but the log "
            f"carries {len(observed)} per-case MEMORY line(s)"
        )
    floor = spec["min_cases"].get(group, 1)
    if cases < floor:
        report.fail(
            f"{suite} group {group}: cases={cases}, below the floor of {floor}. AAP "
            f"0.8.4's memory target is measured over the level x memLevel matrix, and a "
            f"pass over fewer cases is not that measurement"
        )
    if over:
        report.fail(
            f"{suite}: {over} case(s) in group {group} exceeded the memory limit of "
            f"{summary.get('limit')}"
        )
    if dirty:
        report.fail(
            f"{suite}: {dirty} case(s) in group {group} reported a dirty allocator "
            f"balance -- a leak, an out-of-order free or a rogue free"
        )

    # Independently of the summary's own tally: every case's ratio, recomputed from the
    # byte counts, because a summary can only count what its per-case loop produced.
    for field in observed:
        port = as_int(field, "port_bytes")
        oracle = as_int(field, "oracle_bytes")
        if port is None or oracle is None or oracle <= 0:
            report.fail(
                f"{suite} group {group} case {field['case']}: port_bytes="
                f"{field.get('port_bytes')} oracle_bytes={field.get('oracle_bytes')} "
                f"cannot be compared"
            )
            continue
        # Integer arithmetic, so there is no rounding to argue about: the same form
        # `crates/libz-rs-sys/tests/gz_memory.rs` uses for the identical budget.
        if port * 100 > oracle * 115:
            report.fail(
                f"{suite} group {group} case {field['case']}: {port} port byte(s) "
                f"against {oracle} reference byte(s) exceeds the AAP 0.8.4 budget of "
                f"{MEMORY_LIMIT:.2f}"
            )


def gate_balances(
    suite: str,
    balances: list[dict[str, str]],
    memory_cases: set[str],
    report: Report,
) -> None:
    """One clean ledger line per side per memory case, and no line counted twice."""
    if memory_cases and not balances:
        report.fail(
            f"{suite}: {len(memory_cases)} memory case(s) were measured and no "
            f"ALLOC-BALANCE line was printed, so the allocator was never checked"
        )
        return

    rows = [(field["side"], field["case"]) for field in balances]
    unique = set(rows)
    if len(rows) != len(unique):
        repeated = sorted({row for row in rows if rows.count(row) > 1})
        report.fail(
            f"{suite}: the allocator ledger repeated "
            f"{', '.join(f'side={side} case={case}' for side, case in repeated)}; "
            f"counting rows alone would have accepted it"
        )

    want = 2 * len(memory_cases)
    if memory_cases and len(unique) != want:
        report.fail(
            f"{suite}: the allocator ledger reported {len(unique)} distinct side/case "
            f"row(s) for {len(memory_cases)} memory case(s); {want} are expected, one "
            f"per case per side ({' and '.join(BALANCE_SIDES)}). A missing line is an "
            f"unmeasured allocator, not a clean one"
        )

    by_case: dict[str, set[str]] = {}
    for side, case in rows:
        by_case.setdefault(case, set()).add(side)
    for case in sorted(memory_cases):
        absent = [side for side in BALANCE_SIDES if side not in by_case.get(case, set())]
        if absent:
            report.fail(
                f"{suite}: memory case {case} has no ALLOC-BALANCE line for side(s) "
                f"{', '.join(absent)}: a balance that is clean on one side and "
                f"unmeasured on the other proves only that the counter ran"
            )

    for field in balances:
        if field["verdict"] != "clean":
            report.fail(
                f"{suite}: allocator balance for side={field['side']} "
                f"case={field['case']} is {field['verdict']} "
                f"(live_bytes={field['live_bytes']} notlifo={field['notlifo']} "
                f"rogue={field['rogue']} refusals={field['refusals']})"
            )


def gate_suite(
    suite: str,
    spec: dict,
    got: dict[str, list[dict[str, str]]],
    root_override: str | None,
    report: Report,
) -> list[tuple[str, str, float]]:
    """Run every check for one suite, returning ``(group, case, ratio)`` decisions."""
    inventory = one(
        suite,
        "GATE-INVENTORY",
        got.get("GATE-INVENTORY", []),
        "Without it the gate cannot know which groups are authoritative.",
        report,
    )
    criterion = one(
        suite,
        "CRITERION",
        got.get("CRITERION", []),
        "The gate decides from criterion's estimates and cannot locate them without it.",
        report,
    )
    if inventory is None or criterion is None:
        return []

    tiers = check_inventory(suite, spec, inventory, report)
    tier2_required = silesia_required(suite, got, report)
    root = root_override if root_override is not None else criterion["root"]
    if not os.path.isdir(root):
        report.fail(
            f"{suite}: the CRITERION line names root={root}, which is not a directory. "
            f"The estimates that decide the AAP 0.8.4 targets live under it"
        )

    ratio_cases: dict[str, list[dict[str, str]]] = {}
    for field in got.get("RATIO", []):
        ratio_cases.setdefault(field["group"], []).append(field)
    memory_cases: dict[str, list[dict[str, str]]] = {}
    for field in got.get("MEMORY", []):
        memory_cases.setdefault(field["group"], []).append(field)

    summaries: dict[str, list[dict[str, str]]] = {}
    for field in got.get("RATIO-SUMMARY", []):
        summaries.setdefault(field["group"], []).append(field)
    memory_summaries: dict[str, list[dict[str, str]]] = {}
    for field in got.get("MEMORY-SUMMARY", []):
        memory_summaries.setdefault(field["group"], []).append(field)

    # Exactly one summary per DECLARED group, including the ones whose gated count is
    # legitimately zero: the suites promise "every group named in GATE-INVENTORY emits
    # exactly one summary line per run", so a missing one means a group did not run.
    undeclared = sorted(set(summaries) - set(tiers))
    if undeclared:
        report.fail(
            f"{suite}: RATIO-SUMMARY line(s) for undeclared group(s) "
            f"{', '.join(undeclared)}; a group absent from GATE-INVENTORY has no "
            f"declared standing, so its verdict cannot be enforced"
        )
    undeclared_memory = sorted(set(memory_summaries) - set(spec["authoritative_memory"]))
    if undeclared_memory:
        report.fail(
            f"{suite}: MEMORY-SUMMARY line(s) for undeclared group(s) "
            f"{', '.join(undeclared_memory)}"
        )

    decisions: list[tuple[str, str, float]] = []
    for group in sorted(tiers):
        found = summaries.get(group, [])
        if not found:
            report.fail(
                f"{suite}: no RATIO-SUMMARY line for group {group}, which "
                f"GATE-INVENTORY declares. The suites emit one per declared group in "
                f"every run, so its absence means the group did not run"
            )
            continue
        if len(found) > 1:
            report.fail(
                f"{suite}: {len(found)} RATIO-SUMMARY lines for group {group}; exactly "
                f"one is expected, and duplicates mean the log was concatenated or the "
                f"group ran twice, so no count in it can be trusted"
            )
            continue
        exempt = group in spec["tier2_ratio"] and not tier2_required
        for case, ratio in gate_ratio_group(
            suite,
            group,
            found[0],
            ratio_cases.get(group, []),
            criterion,
            root,
            tiers[group],
            exempt,
            spec,
            report,
        ):
            decisions.append((group, case, ratio))

    for group in sorted(spec["authoritative_memory"]):
        found = memory_summaries.get(group, [])
        if not found:
            report.fail(
                f"{suite}: no MEMORY-SUMMARY line for group {group}. That group carries "
                f"AAP 0.8.4's 15% per-stream memory target, so a run without it has not "
                f"measured what this job exists to check"
            )
            continue
        if len(found) > 1:
            report.fail(
                f"{suite}: {len(found)} MEMORY-SUMMARY lines for group {group}; exactly "
                f"one is expected"
            )
            continue
        gate_memory_group(suite, group, found[0], memory_cases.get(group, []), spec, report)

    measured = {field["case"] for group in spec["authoritative_memory"] for field in memory_cases.get(group, [])}
    gate_balances(suite, got.get("ALLOC-BALANCE", []), measured, report)

    for group in sorted(set(ratio_cases) - set(tiers)):
        report.fail(
            f"{suite}: per-case RATIO line(s) for undeclared group {group}"
        )
    for group in sorted(set(memory_cases) - set(spec["authoritative_memory"])):
        report.fail(
            f"{suite}: per-case MEMORY line(s) for undeclared group {group}"
        )

    return decisions


def main(argv: list[str]) -> int:
    """Gate the log named on the command line.  See the module docstring."""
    root_override: str | None = None
    positional: list[str] = []
    index = 1
    while index < len(argv):
        argument = argv[index]
        if argument == "--criterion-root":
            index += 1
            if index >= len(argv):
                print("::error::--criterion-root needs a directory")
                return 2
            root_override = argv[index]
        elif argument.startswith("-"):
            print(f"::error::unrecognised option {argument}")
            return 2
        else:
            positional.append(argument)
        index += 1

    if len(positional) != 1:
        print(f"usage: {os.path.basename(argv[0])} <bench log> [--criterion-root <dir>]")
        return 2
    log = positional[0]

    report = Report()
    seen = parse(log, report)

    if not seen:
        report.fail(
            f"{log} carries no bench gate lines at all: nothing was measured, so there "
            f"is nothing to gate. A truncated or empty log looks exactly like this"
        )

    unknown = sorted(set(seen) - set(SUITES))
    if unknown:
        report.fail(
            f"unrecognised bench suite(s) {', '.join(unknown)} emitted gate lines. A new "
            f"suite must be wired into this gate's SUITES table with its authoritative "
            f"groups, otherwise its measurements would be reported and never enforced"
        )

    decisions: list[tuple[str, str, str, float]] = []
    for suite in sorted(SUITES):
        got = seen.get(suite)
        if got is None:
            report.fail(
                f"{suite} printed no gate line at all. Both throughput suites run in "
                f"every `cargo bench` invocation, so a missing one is a suite that did "
                f"not build or did not start"
            )
            continue
        for group, case, ratio in gate_suite(suite, SUITES[suite], got, root_override, report):
            decisions.append((suite, group, case, ratio))

    print(f"benchmark gate over {log}")
    print(f"  criterion decisions: {len(decisions)} gated case(s) of the authoritative groups")
    for suite, group, case, ratio in sorted(decisions):
        mark = "OVER" if ratio > RATIO_LIMIT else "ok"
        print(f"    {mark:<4} {suite:<14} {group:<22} {case:<28} ratio={ratio:.3f}")
    for note in report.notes:
        print(f"::notice::{note}")
    for warning in report.warnings:
        print(f"::warning::{warning}")

    if report.failures:
        for failure in report.failures:
            print(f"::error::{failure}")
        print()
        print(
            "Only OUTPUT-NEUTRAL remedies are admissible for a throughput shortfall, "
            "because the differential job asserts byte-identical output: the `simd` "
            "feature for the checksum backends, memory-copy strategy, branch layout, "
            "fat LTO with one codegen unit at opt-level 3, and the "
            "-Cllvm-args=-enable-dfa-jump-thread codegen flag. Changing match "
            "selection, the lazy-match threshold, hash-chain traversal or Huffman "
            "tie-breaking is NOT admissible however much faster it would be: every one "
            "of those changes the emitted bytes."
        )
        print(f"benchmark regression: FAIL -- {len(report.failures)} failure(s)")
        return 1

    print(
        f"benchmark regression: PASS -- {len(decisions)} gated case(s) decided from "
        f"criterion's estimates, all within the AAP limits, "
        f"{len(report.warnings)} warning(s)"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
