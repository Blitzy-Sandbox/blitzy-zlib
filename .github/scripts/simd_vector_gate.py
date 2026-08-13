#!/usr/bin/env python3
"""Measure the vector-instruction content of the `simd` feature's two checksum backends, and fail when it disagrees with what the tree claims.

# What this closes

AAP 0.5.1.4 describes the `simd` feature as enabling "vectorized CRC-32 and Adler-32
backends", and AAP 0.8.4 lists "checksum vectorization behind the `simd` feature" among the
admissible output-neutral throughput levers. Whether a backend is *actually* vectorized is a
property of the compiled object, not of its file name or of a sentence in its module
documentation -- and for one of the two it was not. A review of the shipped `x86_64` release
library found `crc32::simd`'s body to contain **zero** vector instructions while the file, the
feature and the backend type were all named `simd`, and no gate anywhere could have noticed.

This script is that gate. It disassembles the release library built with `--features simd`,
locates each checksum backend's own symbol, and counts instructions and vector instructions in
each. Two things then have to hold, and both are asserted rather than described:

* the Adler-32 backend **must** contain vector instructions, above a floor -- so the claim that
  it is vectorized is a measurement;
* every backend's measured vector content must match the figure this script is told to expect --
  so neither an accidental loss of vectorization nor a *gain* of it can happen silently. A gain
  is a documentation change, not a free pass: it means a claim in the tree became true and has
  to be rewritten.

# What it does not assert

Throughput. `benches/checksum_bench.rs` and `.github/scripts/bench_gate.py` own that, and they
own it the right way round: a backend that is slower than the one it replaces fails there
regardless of how many vector instructions it contains. Vector content and speed are separate
properties and this script measures exactly one of them.

Nor does it assert that a *particular* instruction set is used. The workspace sets no
`-C target-cpu` anywhere -- there is no `.cargo/config.toml`, and neither `Cargo.toml` nor
`rust-toolchain.toml` raises the baseline -- so what a given host emits is whatever the triple's
default permits. On `x86_64` that is SSE2, which is part of the architecture.

# Why CRC-32's expected figure is zero, and why that is recorded rather than fixed

A genuinely vectorized CRC-32 is not reachable from `crates/zlib-rs`, and the reason is
structural rather than a matter of effort:

* `core::arch` is unreachable at *every released version*, not merely at the declared MSRV. The
  crate carries `#![forbid(unsafe_code)]` (AAP 0.7.1 (a)), which rejects even the declaration of
  an `unsafe fn`; and -- verified by compiling against stable 1.97.1 -- the load and store
  intrinsics are still `unsafe fn` because they dereference a raw pointer, while calling any
  `#[target_feature]` function from ordinary code is itself unsafe. Wrapping the kernel in
  `#[target_feature(enable = "sse2")]` only relocates the rejection to the call site;
* `core::simd`, the portable vector API AAP 0.2.2.2 names, is the mechanism that *would* work --
  the lane-parallel shape compiles under `#![forbid(unsafe_code)]` with no raw pointer and emits
  25 vector instructions -- and is blocked by stabilization alone: `portable_simd` is still
  unstable on 1.99.0-nightly against a stable-channel MSRV of 1.80 (AAP 0.7.1 (h));
* autovectorization cannot help, because a table-driven CRC needs a **gather** -- 256-entry rows
  indexed by a byte -- and portable Rust has no gather. What is left after the loads is a
  handful of exclusive-ors that LLVM correctly declines to vectorize, since building vectors
  from four unrelated scalar loads costs more than the three XORs it would save.

The measured alternative was written and timed rather than assumed. A table-free, lane-parallel
formulation over sixteen contiguous blocks -- the shape that *does* vectorize, and it does:
92 vector instructions of 398 -- runs at 1.93 to 2.44 ns/byte against the table braid's 0.24 to
0.32 ns/byte on the same host, which is **6 to 9 times slower**. Shipping it would trade a
measured throughput lever for a measured throughput loss in order to satisfy a description.

So the CRC-32 expectation is zero, the reason is recorded here and in
`crates/zlib-rs/src/crc32/simd.rs`, and the gate's job is to make sure the number and the
claim stay married. If a future toolchain makes real vector code reachable -- a stabilized
`portable_simd` would do it; raising the MSRV alone provably would not -- this gate fails on the
*increase* and points at the sentences to rewrite.

# Usage

    python3 .github/scripts/simd_vector_gate.py --library target/simd/release/libz.so

    python3 .github/scripts/simd_vector_gate.py --library <path> \\
        --expect adler32=vectorized --expect crc32=scalar --floor 16

`--expect NAME=vectorized` requires a non-zero vector count for that backend and at least
`--floor` vector instructions; `NAME=scalar` requires exactly zero. Unknown names are an error,
so a renamed backend cannot silently drop out of the measurement.

Exit status is 0 when every expectation holds and 1 otherwise, with the measured table printed
either way so a failing run says what it saw and not merely that it failed.
"""

from __future__ import annotations

import argparse
import platform
import re
import shutil
import subprocess
import sys
from dataclasses import dataclass, field

#: Rust mangles module paths into the symbol, so the backend's own function is found by the
#: module path fragment rather than by a demangled name -- `objdump` is not asked to demangle,
#: because its ability to demangle v0 Rust symbols varies by binutils version and the fragment
#: is stable across all of them.
BACKENDS: dict[str, tuple[str, ...]] = {
    # `adler32::simd::adler32_blocks` is the vectorized kernel; `accumulate_block` is inlined
    # into it. Both fragments are accepted so that a future split of the kernel is still found.
    "adler32": ("adler324simd",),
    # `crc32::simd::fold_braided_body`, deliberately not `#[inline]` so that it has a symbol of
    # its own to measure -- see that module's documentation.
    "crc32": ("crc324simd",),
}

#: Per-architecture vector-register patterns. The gate refuses to guess on an architecture it
#: does not know, because a pattern that matched nothing would report "scalar" for everything
#: and turn a green run into a lie.
VECTOR_PATTERNS: dict[str, re.Pattern[str]] = {
    "x86_64": re.compile(r"%(?:x|y|z)mm[0-9]+"),
    "i686": re.compile(r"%(?:x|y|z)mm[0-9]+"),
    "i386": re.compile(r"%(?:x|y|z)mm[0-9]+"),
    "aarch64": re.compile(r"\b[vq][0-9]+\.(?:[0-9]+[bhsd]|[bhsd])\b"),
}

#: `objdump -d` starts a function body with `<address> <symbol>:`.
SYMBOL_LINE = re.compile(r"^[0-9a-f]+\s+<(?P<name>.+)>:\s*$")

#: A disassembled instruction line: address, tab, encoded bytes, tab, mnemonic and operands.
INSTRUCTION_LINE = re.compile(r"^\s+[0-9a-f]+:\s")


@dataclass
class Measurement:
    """What one backend's compiled body was found to contain."""

    #: Every symbol whose name carried the backend's module fragment.
    symbols: list[str] = field(default_factory=list)
    #: Total disassembled instructions across those symbols.
    instructions: int = 0
    #: Of those, the ones naming a vector register.
    vector: int = 0

    @property
    def found(self) -> bool:
        """Whether any symbol was located at all."""
        return bool(self.symbols)


def disassemble(library: str) -> list[str]:
    """Return `objdump -d`'s output for `library`, as lines."""
    objdump = shutil.which("objdump")
    if objdump is None:
        raise SystemExit("FAIL: objdump is not on PATH, so vector content cannot be measured")
    completed = subprocess.run(
        [objdump, "-d", "--no-show-raw-insn", library],
        capture_output=True,
        text=True,
        check=False,
    )
    if completed.returncode != 0:
        raise SystemExit(
            f"FAIL: objdump -d {library} exited {completed.returncode}: "
            f"{completed.stderr.strip()[:400]}"
        )
    return completed.stdout.splitlines()


def measure(lines: list[str], vector: re.Pattern[str]) -> dict[str, Measurement]:
    """Attribute every instruction in `lines` to the backend whose fragment its symbol carries."""
    found = {name: Measurement() for name in BACKENDS}
    current: str | None = None

    for line in lines:
        symbol = SYMBOL_LINE.match(line)
        if symbol is not None:
            name = symbol.group("name")
            current = None
            for backend, fragments in BACKENDS.items():
                if any(fragment in name for fragment in fragments):
                    current = backend
                    found[backend].symbols.append(name)
                    break
            continue
        if current is None:
            continue
        if INSTRUCTION_LINE.match(line) is None:
            continue
        found[current].instructions += 1
        if vector.search(line) is not None:
            found[current].vector += 1

    return found


def report(measured: dict[str, Measurement], machine: str) -> None:
    """Print the measured table, whatever the verdict."""
    print(f"host architecture: {machine}")
    print(f"{'backend':<10} {'symbols':>7} {'insns':>7} {'vector':>7}  verdict")
    for name, found in measured.items():
        verdict = "not found" if not found.found else (
            "vectorized" if found.vector else "scalar"
        )
        print(
            f"{name:<10} {len(found.symbols):>7} {found.instructions:>7} "
            f"{found.vector:>7}  {verdict}"
        )


def main() -> int:
    """Measure and check. Returns the process exit status."""
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument(
        "--library",
        required=True,
        help="the release library built with --features simd",
    )
    parser.add_argument(
        "--expect",
        action="append",
        default=[],
        metavar="NAME=vectorized|scalar",
        help="what a backend must be; repeatable. Defaults to "
        "adler32=vectorized and crc32=scalar.",
    )
    parser.add_argument(
        "--floor",
        type=int,
        default=16,
        help="the fewest vector instructions a 'vectorized' backend may contain (default 16)",
    )
    arguments = parser.parse_args()

    machine = platform.machine()
    vector = VECTOR_PATTERNS.get(machine)
    if vector is None:
        print(
            f"SKIP: no vector-register pattern for architecture {machine!r}; "
            "add one to VECTOR_PATTERNS rather than letting this report 'scalar'"
        )
        return 0

    expectations = {"adler32": "vectorized", "crc32": "scalar"}
    if arguments.expect:
        expectations = {}
        for item in arguments.expect:
            name, _, want = item.partition("=")
            if name not in BACKENDS:
                print(f"FAIL: {name!r} is not a known backend; known: {sorted(BACKENDS)}")
                return 1
            if want not in {"vectorized", "scalar"}:
                print(f"FAIL: {item!r} must end in =vectorized or =scalar")
                return 1
            expectations[name] = want

    measured = measure(disassemble(arguments.library), vector)
    report(measured, machine)

    failures: list[str] = []
    for name, want in expectations.items():
        found = measured[name]
        if not found.found:
            failures.append(
                f"{name}: no symbol carrying {BACKENDS[name]} was found in the library. "
                "Either the backend was renamed -- update BACKENDS -- or it was inlined away, "
                "in which case give it a symbol of its own so it can be measured."
            )
            continue
        if want == "vectorized":
            if found.vector == 0:
                failures.append(
                    f"{name}: expected a vectorized backend and found {found.instructions} "
                    "instructions with NONE naming a vector register. Either the "
                    "vectorization was lost or the expectation is now wrong; do not silence "
                    "this by changing the expectation without changing what the tree claims."
                )
            elif found.vector < arguments.floor:
                failures.append(
                    f"{name}: {found.vector} vector instruction(s) is below the floor of "
                    f"{arguments.floor}, which is too few to be a throughput lever rather "
                    "than an incidental spill or a memcpy."
                )
        elif found.vector != 0:
            failures.append(
                f"{name}: expected a scalar backend and found {found.vector} vector "
                f"instruction(s) of {found.instructions}. This is not a failure of the code "
                "-- it means a claim in the tree became TRUE. Update "
                f"`crates/zlib-rs/src/{name}/simd.rs`, that backend's `NAME`, `README`, "
                "`rust/README.md` and this script's expectation together, and confirm the "
                "change against `benches/checksum_bench.rs` before doing so."
            )

    if failures:
        print()
        for failure in failures:
            print(f"FAIL: {failure}")
        return 1

    described = ", ".join(f"{name}={want}" for name, want in sorted(expectations.items()))
    print(f"\nPASS: measured vector content matches the expectation ({described}).")
    return 0


if __name__ == "__main__":
    sys.exit(main())
