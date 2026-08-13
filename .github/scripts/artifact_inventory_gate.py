#!/usr/bin/env python3
"""Derive the crate inventory and the dynamic import surface of a packaged library from the library itself, and fail when either disagrees with what this tree declares.

# What this closes

`deny.toml` is the tree's supply-chain authority, and it is a good one -- it bans
`miniz_oxide` by name, along with every other third-party DEFLATE implementation, so that a
second compressor cannot enter the graph and answer for this one. But cargo-deny reads
manifests and `Cargo.lock`, and the shipped artifact is not assembled from those alone: it is
relinked out of a Rust `staticlib`, which carries members of the **precompiled `std`
sysroot**. Those members are in no lock file, cargo-deny cannot see them, and a review of the
packaged `libz.so.1.3.2.1-motley` found `miniz_oxide::inflate::core::decompress` and
`adler2::Adler32::write_slice` inside it -- a whole second DEFLATE decoder in a library whose
own reason for existing is the first one, banned by name three files away and shipped anyway.

The same blind spot covered the import side. The whole-archive artifact declared 183 dynamic
dependencies, among them `posix_spawnp`, `execvp`, `fork`, `socket`, `connect`, `chroot`,
`setuid`, `dlsym` and `getrandom`. A compression library reaches none of those, and a
distribution auditing what `libz.so.1` may do would have had to account for every one.

This script closes both by asking the *artifact*. It reads the symbol table of a built
library or archive, recovers each symbol's originating crate from Rust's `v0` mangling,
recovers the dynamic imports, and compares both against the declarations below. Nothing here
is inferred from a manifest, so nothing here can be fooled by one.

# The three ways it fails, and why each is a failure rather than a note

* **An undeclared crate is present.** Something entered the artifact that no one wrote down.
  That is the `miniz_oxide` case exactly, and the remedy is either to stop linking it or to
  declare it here with its licence and the reason it is reachable.
* **A declared crate is absent.** Declarations rot in the direction of over-claiming, and a
  list that names code the artifact no longer contains quietly becomes fiction. Entries
  marked `required` must be found.
* **A forbidden import is present.** Process creation, privilege change, filesystem-root
  change and network access are not part of any zlib entry point. Their appearance means
  either that `std` machinery nothing reaches was linked in, or that the port grew a
  capability it should not have.

# What it does not do

It does not replace cargo-deny; it covers what cargo-deny structurally cannot see. Run both.
It also does not attempt to police `core`, `alloc` and `compiler_builtins` beyond declaring
them: they are the language runtime, and a Rust artifact without them is not a thing that
exists.

Reachability is not asserted here either -- it is established by the packaging step, which
links with `--gc-sections` after demand-driven member selection. What survives that is
referenced from an exported entry point, so this script can describe the surviving set as
reachable without having to prove it again.

# Usage

    python3 .github/scripts/artifact_inventory_gate.py \\
        --library target/dropin/libz.so.1.3.2.1-motley

    python3 .github/scripts/artifact_inventory_gate.py --library <path> --archive <path.a>

`--archive` widens the crate check to a static archive, where member selection has happened
but no link-time pruning has; imports are only checked on a shared library, since an archive
has no dynamic dependencies. `--nm` names the reader when it is not `nm` on `PATH`.

Exit status is 0 when every check holds and 1 otherwise, with the measured tables printed
either way so that a failing run says what it saw.
"""

from __future__ import annotations

import argparse
import re
import shutil
import subprocess
import sys
from dataclasses import dataclass, field


@dataclass(frozen=True)
class Declared:
    """One crate this tree accepts inside a packaged artifact.

    ``licences`` is the SPDX expression the crate publishes, recorded here because the sysroot
    members never appear in a `cargo deny list` and this file is therefore the only place their
    licensing is written down. ``required`` marks the ones whose absence means this script is
    reading the wrong file or that the declaration has gone stale.
    """

    licences: str
    reason: str
    required: bool = False


#: Every crate permitted in the packaged library, with why it is there.
#:
#: The port's own two crates and the language runtime are required; everything else is
#: `std`-sourced and reachable only through `std`'s default panic hook, which symbolises a
#: backtrace before the process dies.  That chain is the whole explanation for the presence of
#: `gimli`, `addr2line`, `object`, `rustc_demangle`, `memchr`, `hashbrown`, `miniz_oxide` and
#: `adler2`, and it cannot be broken while `std` is linked: removing it needs
#: `panic_immediate_abort`, which needs a rebuilt `std`, which needs nightly `-Zbuild-std`.
#: `panic = "abort"` does not help -- the hook still runs before the abort.
INVENTORY: dict[str, Declared] = {
    "zlib_rs": Declared(
        "Zlib", "the port's safe core; this is the library", required=True
    ),
    "z": Declared(
        "Zlib",
        "★ the port's C ABI facade -- package `libz-rs-sys`, whose `[lib] name` is `z` so that "
        "cargo emits libz.so and libz.a.  The mangling records the LIB TARGET name, not the "
        "package name, so this entry is spelled `z`; there is no crate called `libz_rs_sys` "
        "inside the artifact and requiring one would fail forever",
        required=True,
    ),
    "__rustc": Declared(
        "MIT OR Apache-2.0",
        "the compiler's own shim crate, holding rust_panic and the __rdl_* allocator hooks",
    ),
    "core": Declared(
        "MIT OR Apache-2.0", "the Rust language core library", required=True
    ),
    "alloc": Declared(
        "MIT OR Apache-2.0", "the Rust allocation library, used through `alloc`", required=True
    ),
    "std": Declared(
        "MIT OR Apache-2.0",
        "file I/O for the gz layer, which is what the `std` feature buys",
        required=True,
    ),
    "compiler_builtins": Declared(
        "MIT OR Apache-2.0",
        "the compiler's own intrinsic implementations; part of every Rust artifact",
    ),
    "unwind": Declared(
        "MIT OR Apache-2.0", "the panic runtime's unwinder shim"
    ),
    "gimli": Declared(
        "MIT OR Apache-2.0",
        "DWARF reader, reached from std's panic hook while symbolising a backtrace",
    ),
    "addr2line": Declared(
        "MIT OR Apache-2.0", "address-to-line mapping for the same backtrace path"
    ),
    "object": Declared(
        "MIT OR Apache-2.0", "object-file reader for the same backtrace path"
    ),
    "rustc_demangle": Declared(
        "MIT OR Apache-2.0", "symbol demangling for the same backtrace path"
    ),
    "memchr": Declared(
        "Unlicense OR MIT", "substring search, used by the object reader"
    ),
    "hashbrown": Declared(
        "MIT OR Apache-2.0", "the hash map std's collections are built on"
    ),
    "miniz_oxide": Declared(
        "MIT OR Zlib OR Apache-2.0",
        "★ a SECOND DEFLATE decoder, and it is here on purpose rather than by oversight: "
        "the object reader decompresses compressed debug sections with it while std's panic "
        "hook symbolises a backtrace. `deny.toml` bans it from the Cargo graph and that ban "
        "stands; this entry records the copy that arrives with the precompiled sysroot, which "
        "cargo-deny cannot see. It is never reached from a zlib entry point, and no zlib "
        "stream is ever decoded by it",
    ),
    "adler2": Declared(
        "MIT OR Apache-2.0", "miniz_oxide's Adler-32; arrives with it and for the same reason"
    ),
}

#: Dynamic imports that must never appear, grouped by the capability they would grant.
#:
#: These are not a style preference.  Each was measured in the whole-archive artifact this
#: gate was written against, and each is a capability a consumer auditing `libz.so.1` would
#: otherwise have to justify.  Demand-driven member selection plus `--gc-sections` removed all
#: of them; this list is what keeps them removed.
FORBIDDEN_IMPORTS: dict[str, tuple[str, ...]] = {
    "process creation": (
        "fork",
        "vfork",
        "execv",
        "execve",
        "execvp",
        "posix_spawn",
        "posix_spawnp",
        "system",
        "popen",
        "pidfd_spawnp",
    ),
    "privilege and root change": (
        "setuid",
        "seteuid",
        "setgid",
        "setegid",
        "setgroups",
        "chroot",
    ),
    "network access": (
        "socket",
        "socketpair",
        "connect",
        "bind",
        "listen",
        "accept",
        "accept4",
        "send",
        "sendto",
        "recv",
        "recvfrom",
        "getaddrinfo",
    ),
    "dynamic code loading": ("dlopen", "dlsym", "dlmopen"),
}

#: A `v0` crate root: `C`, an optional `s<base-62>_` disambiguator, then a length-prefixed name.
#:
#: The shape is exact rather than approximate, and that matters more than it looks.  A first
#: attempt allowed `C[sh]` and a lazy run of any identifier character, which matched the `Ch` of
#: `Chains`, `Chars` and `CharSearcher` in ordinary `core` symbols and invented the crates
#: `deflate`, `iter` and `Searcher` out of the *following* path component's length prefix.  A
#: gate that reports crates the artifact does not contain is worse than no gate: it trains its
#: reader to add declarations for fictions.  So the disambiguator is base-62 only -- no
#: underscore, per the mangling grammar -- and the alternative without one requires a digit
#: immediately after the `C`.
_V0_CRATE = re.compile(r"C(?:s[0-9a-zA-Z]*_)?(u?)([0-9]+)")

#: `_ZN<len><name>` -- the legacy scheme's first path component is the crate.
_LEGACY_CRATE = re.compile(r"^_?_ZN([0-9]+)")

#: What a crate name may contain once decoded.
_IDENT = re.compile(r"[A-Za-z_][A-Za-z0-9_]*")


def _identifier_at(symbol: str, start: int, length: int) -> str | None:
    """Reads one `v0` length-prefixed identifier body, honouring the optional `_` separator.

    The grammar emits a `_` between the length and the bytes exactly when the name begins with
    an underscore, so that the reader cannot mistake the name's leading `_` for more digits.
    `7___rustc` is therefore length 7 and the name `__rustc`, not length 7 and `___rust` --
    which is what a parser that ignores the separator reports, and did.
    """

    if start < len(symbol) and symbol[start] == "_":
        start += 1

    name = symbol[start : start + length]
    if len(name) != length or _IDENT.fullmatch(name) is None:
        return None
    return name


def crates_in(symbol: str) -> set[str]:
    """Recover every crate name mentioned by one mangled symbol.

    A single symbol names more than one crate whenever it is a generic instantiated across a
    boundary -- `core::slice::copy_within` monomorphised inside `miniz_oxide` names both -- so
    this returns a set rather than one name, and the caller unions them. A name that parses to
    nothing contributes nothing: `GCC_except_table*`, `DW.ref.*` and the port's own unmangled
    exports are not crate-attributable and are counted separately.
    """

    found: set[str] = set()

    # Only `v0`-mangled names are scanned for embedded crate roots.  In anything else a `C`
    # followed by digits is just a `C` followed by digits.
    if symbol.startswith("_R") or symbol.startswith("R"):
        for match in _V0_CRATE.finditer(symbol):
            # `u` marks a punycode identifier.  Crate names that need one are vanishingly rare
            # and decoding punycode here would add a dependency for no measured benefit, so the
            # raw bytes are recorded and a human reads the result.
            name = _identifier_at(symbol, match.end(), int(match.group(2)))
            if name is not None:
                found.add(name)

    legacy = _LEGACY_CRATE.match(symbol)
    if legacy is not None:
        name = _identifier_at(symbol, legacy.end(), int(legacy.group(1)))
        if name is not None:
            found.add(name)

    return found


@dataclass
class Report:
    """Collects failures so that one run reports every problem rather than only the first."""

    failures: list[str] = field(default_factory=list)

    def fail(self, message: str) -> None:
        """Records one failure."""
        self.failures.append(message)

    def finish(self, subject: str) -> int:
        """Prints the verdict and returns the process exit status."""
        if not self.failures:
            print(f"PASS: {subject}")
            return 0

        print()
        for index, message in enumerate(self.failures, start=1):
            print(f"FAIL {index}: {message}")
        return 1


def read_symbols(nm: str, path: str, extra: tuple[str, ...]) -> list[str]:
    """Returns the symbol names `nm` reports for `path`, or exits when it cannot be read."""

    command = [nm, *extra, path]
    try:
        completed = subprocess.run(
            command, check=True, capture_output=True, text=True
        )
    except FileNotFoundError:
        print(f"error: {nm} is not executable; name the reader with --nm", file=sys.stderr)
        raise SystemExit(1) from None
    except subprocess.CalledProcessError as error:
        print(
            f"error: {' '.join(command)} failed with status {error.returncode}:\n"
            f"{error.stderr.strip()}",
            file=sys.stderr,
        )
        raise SystemExit(1) from None

    names = []
    for line in completed.stdout.splitlines():
        fields = line.split()
        if fields:
            names.append(fields[-1])
    return names


def bare(symbol: str) -> str:
    """Strips a versioned import's `@VERSION` suffix, so `socket@GLIBC_2.2.5` reads as `socket`."""

    return symbol.split("@", 1)[0]


def main() -> int:
    """Parses arguments, measures the artifact, and reports."""

    parser = argparse.ArgumentParser(
        description="Check a packaged library's crate inventory and import surface "
        "against this tree's declarations."
    )
    parser.add_argument(
        "--library",
        required=True,
        help="the packaged shared library, normally target/dropin/libz.so.<version>",
    )
    parser.add_argument(
        "--archive", help="an optional static archive to include in the crate check"
    )
    parser.add_argument("--nm", default="nm", help="the symbol reader to use (default: nm)")
    arguments = parser.parse_args()

    if shutil.which(arguments.nm) is None:
        print(
            f"error: {arguments.nm} is not on PATH.  This gate reads the built artifact, so "
            f"a symbol reader is a requirement rather than an optional extra; install "
            f"binutils or pass --nm.",
            file=sys.stderr,
        )
        return 1

    report = Report()

    # ---- crates -----------------------------------------------------------------
    counts: dict[str, int] = {}
    unattributed = 0
    sources = [("library", arguments.library)]
    if arguments.archive:
        sources.append(("archive", arguments.archive))

    for _kind, path in sources:
        for symbol in read_symbols(arguments.nm, path, ("--defined-only",)):
            names = crates_in(symbol)
            if not names:
                unattributed += 1
            for name in names:
                counts[name] = counts.get(name, 0) + 1

    print(f"crate inventory of {arguments.library}")
    print(f"{'crate':<20} {'symbols':>8}  {'licence':<28} status")
    for name in sorted(counts, key=lambda key: (-counts[key], key)):
        entry = INVENTORY.get(name)
        status = "declared" if entry is not None else "*** UNDECLARED ***"
        licence = entry.licences if entry is not None else "unknown"
        print(f"{name:<20} {counts[name]:>8}  {licence:<28} {status}")
    print(f"{'(not attributable)':<20} {unattributed:>8}")

    for name in sorted(counts):
        if name not in INVENTORY:
            report.fail(
                f"the artifact contains code from `{name}`, which this tree does not "
                f"declare ({counts[name]} symbol(s)).  Either stop linking it, or add it to "
                f"INVENTORY in this script with its licence and the reason it is reachable.  "
                f"Do not add it silently: cargo-deny cannot see sysroot members, so this "
                f"list is the only record of them."
            )

    for name, entry in sorted(INVENTORY.items()):
        if entry.required and name not in counts:
            report.fail(
                f"`{name}` is declared as required but the artifact contains none of it.  "
                f"Either this is not the packaged library, or the declaration has gone "
                f"stale -- and a declaration that names code the artifact no longer holds is "
                f"how an inventory becomes fiction."
            )

    # ---- imports ----------------------------------------------------------------
    imports = {
        bare(symbol)
        for symbol in read_symbols(
            arguments.nm, arguments.library, ("-D", "--undefined-only")
        )
    }
    print()
    print(f"dynamic imports: {len(imports)}")

    for capability, names in sorted(FORBIDDEN_IMPORTS.items()):
        present = sorted(name for name in names if name in imports)
        verdict = "absent" if not present else "*** PRESENT: " + ", ".join(present)
        print(f"  {capability:<26} {verdict}")
        if present:
            report.fail(
                f"the library imports {', '.join(present)}, granting it {capability}.  No "
                f"zlib entry point reaches any of these; their presence means unreachable "
                f"`std` machinery was linked in, or that the port grew a capability it must "
                f"not have.  Check that member selection is demand-driven and that the final "
                f"link still passes --gc-sections."
            )

    return report.finish(
        f"{len(counts)} crate(s) present, all declared; "
        f"{len(imports)} dynamic import(s), none forbidden"
    )


if __name__ == "__main__":
    sys.exit(main())
