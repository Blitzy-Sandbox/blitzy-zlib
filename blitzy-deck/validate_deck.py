#!/usr/bin/env python3
"""Validate the Blitzy executive reveal.js deck against its acceptance contract.

This is the mandated structural validator for ``executive-summary.html``. It
enforces, on a self-contained reveal.js deck, the requirements drawn from the
Agent Action Plan's "Executive Presentation" rule and the final-acceptance
checkpoint:

  1. Slide count and slide-type distribution — exactly 16 ``<section>`` slides
     made up of 1 title, 6 section dividers, 8 content slides and 1 closing.
  2. Every slide carries at least one non-text visual (a Mermaid diagram, a KPI
     card, a styled table, or a Lucide SVG icon).
  3. Zero emoji anywhere in the slide text (Lucide SVG icons only).
  4. Each content slide stays within the limits of at most 4 bullets and at most
     40 words of body text.
  5. The three CDN dependencies are pinned to the required versions
     (reveal.js 5.1.0, Mermaid 11.4.0, Lucide 0.460.0).
  6. The Blitzy brand palette tokens are present.
  7. Mermaid is initialised with ``startOnLoad: false`` and both
     ``mermaid.run()`` and ``lucide.createIcons()`` are wired up.

Usage::

    python3 blitzy-deck/validate_deck.py [path/to/executive-summary.html]

When no path is given the sibling ``executive-summary.html`` is validated. The
script prints a per-check ``[PASS]``/``[FAIL]`` report and exits 0 only when
every check passes, otherwise it exits 1.
"""

from __future__ import annotations

import os
import sys
from html.parser import HTMLParser

# --------------------------------------------------------------------------- #
# Acceptance constants
# --------------------------------------------------------------------------- #

EXPECTED_TOTAL = 16
EXPECTED_DISTRIBUTION = {"title": 1, "divider": 6, "content": 8, "closing": 1}

MAX_BULLETS_PER_CONTENT_SLIDE = 4
MAX_BODY_WORDS_PER_CONTENT_SLIDE = 40

PINNED_CDNS = ["reveal.js@5.1.0", "mermaid@11.4.0", "lucide@0.460.0"]

# The Blitzy brand palette (primary, dark, teal accent, navy, gradient stops).
PALETTE_TOKENS = ["#5B39F3", "#2D1C77", "#94FAD5", "#1A105F", "#7A6DEC", "#4101DB"]

# JavaScript wiring that must be present for diagrams/icons to render.
REQUIRED_WIRING = ["startOnLoad: false", "mermaid.run", "lucide.createIcons"]

# Unicode ranges that denote pictographic emoji. Deliberately EXCLUDES ordinary
# typographic punctuation such as the em dash (U+2014) and middle dot (U+00B7),
# which are legitimate body text and must never be flagged.
_EMOJI_RANGES = (
    (0x1F000, 0x1FAFF),  # emoji, pictographs, transport, supplemental symbols
    (0x2600, 0x26FF),    # miscellaneous symbols
    (0x2700, 0x27BF),    # dingbats
    (0x2B00, 0x2BFF),    # miscellaneous symbols and arrows (stars, etc.)
    (0xFE00, 0xFE0F),    # variation selectors (emoji presentation)
)

# Element tags whose text is structural/diagram content, NOT prose "body text".
_SUPPRESS_TAGS = {"h1", "h2", "h3", "pre", "table", "script", "style", "svg"}
# Class-name substrings whose subtree is a visual component, NOT prose.
_SUPPRESS_CLASS_SUBSTRINGS = (
    "eyebrow",
    "kpi-grid",
    "kpi-card",
    "kpi-value",
    "kpi-label",
    "kpi-icon",
)


def _is_emoji(char: str) -> bool:
    """Return ``True`` if ``char`` is a pictographic emoji codepoint."""
    code = ord(char)
    return any(low <= code <= high for low, high in _EMOJI_RANGES)


class _Slide:
    """Accumulated facts about a single ``<section>`` slide."""

    __slots__ = (
        "kind",
        "bullets",
        "body_words",
        "has_mermaid",
        "has_table",
        "has_kpi",
        "has_lucide",
        "text",
    )

    def __init__(self, kind: str) -> None:
        self.kind = kind
        self.bullets = 0
        self.body_words = 0
        self.has_mermaid = False
        self.has_table = False
        self.has_kpi = False
        self.has_lucide = False
        self.text: list[str] = []

    @property
    def has_visual(self) -> bool:
        return self.has_mermaid or self.has_table or self.has_kpi or self.has_lucide


class _DeckParser(HTMLParser):
    """Stream-parse the deck, collecting per-slide facts.

    Body-word accounting uses a suppression stack: text is only counted as prose
    when none of its ancestor elements is a heading, an eyebrow label, or a
    visual container (Mermaid ``<pre>``, ``<table>``, KPI card). This lets
    diagram source, table cells and KPI figures count toward the visual
    requirement without inflating the prose word count.
    """

    def __init__(self) -> None:
        super().__init__(convert_charrefs=True)
        self.slides: list[_Slide] = []
        self._current: _Slide | None = None
        # Stack of (tag, suppress_flag) for currently-open elements.
        self._stack: list[tuple[str, bool]] = []

    def handle_starttag(self, tag, attrs):  # noqa: D401 - HTMLParser hook
        attr = dict(attrs)
        cls = attr.get("class", "") or ""

        if tag == "section":
            kind = "content"
            if "slide-title" in cls:
                kind = "title"
            elif "slide-divider" in cls:
                kind = "divider"
            elif "slide-closing" in cls:
                kind = "closing"
            self._current = _Slide(kind)
            self.slides.append(self._current)
            self._stack = []
            return

        if self._current is None:
            return

        # Visual detection.
        if tag == "pre" and "mermaid" in cls:
            self._current.has_mermaid = True
        if tag == "table":
            self._current.has_table = True
        if "kpi-card" in cls:
            self._current.has_kpi = True
        if tag == "i" and "data-lucide" in attr:
            self._current.has_lucide = True
        if tag == "li":
            self._current.bullets += 1

        suppress = tag in _SUPPRESS_TAGS or any(
            sub in cls for sub in _SUPPRESS_CLASS_SUBSTRINGS
        )
        inherited = any(flag for _, flag in self._stack)
        self._stack.append((tag, suppress or inherited))

    def handle_endtag(self, tag):  # noqa: D401 - HTMLParser hook
        if tag == "section":
            self._current = None
            self._stack = []
            return
        # Pop back to (and including) the most recent matching open tag. This is
        # robust for the well-formed, fully-closed markup of the deck.
        for index in range(len(self._stack) - 1, -1, -1):
            if self._stack[index][0] == tag:
                del self._stack[index:]
                break

    def handle_data(self, data):  # noqa: D401 - HTMLParser hook
        if self._current is None:
            return
        self._current.text.append(data)
        if data.strip() and not any(flag for _, flag in self._stack):
            self._current.body_words += len(data.split())


class _Report:
    """Collects pass/fail results and renders a final report."""

    def __init__(self) -> None:
        self._results: list[tuple[bool, str, list[str]]] = []

    def check(self, ok: bool, title: str, details: list[str] | None = None) -> None:
        self._results.append((bool(ok), title, details or []))

    @property
    def passed(self) -> bool:
        return all(ok for ok, _, _ in self._results)

    def render(self) -> str:
        lines: list[str] = []
        for ok, title, details in self._results:
            lines.append(f"[{'PASS' if ok else 'FAIL'}] {title}")
            for detail in details:
                lines.append(f"         - {detail}")
        verdict = "VALID" if self.passed else "INVALID"
        lines.append("")
        lines.append(f"RESULT: {verdict}")
        return "\n".join(lines)


def validate(html_path: str) -> _Report:
    """Run every acceptance check against ``html_path`` and return a report."""
    report = _Report()

    with open(html_path, "r", encoding="utf-8") as handle:
        source = handle.read()

    parser = _DeckParser()
    parser.feed(source)
    slides = parser.slides

    # (1) Slide count and distribution.
    distribution = {"title": 0, "divider": 0, "content": 0, "closing": 0}
    for slide in slides:
        distribution[slide.kind] += 1
    count_ok = len(slides) == EXPECTED_TOTAL and distribution == EXPECTED_DISTRIBUTION
    report.check(
        count_ok,
        f"Slide count and distribution (expected {EXPECTED_TOTAL}: "
        f"{EXPECTED_DISTRIBUTION})",
        [f"found {len(slides)} sections, distribution {distribution}"],
    )

    # (2) Every slide carries at least one non-text visual.
    missing_visual = [
        i + 1 for i, slide in enumerate(slides) if not slide.has_visual
    ]
    report.check(
        not missing_visual,
        "Every slide has at least one non-text visual "
        "(Mermaid / KPI card / table / Lucide icon)",
        [] if not missing_visual else [f"slides without a visual: {missing_visual}"],
    )

    # (3) Zero emoji in slide text.
    emoji_hits: list[str] = []
    for i, slide in enumerate(slides):
        text = "".join(slide.text)
        found = sorted({ch for ch in text if _is_emoji(ch)})
        if found:
            emoji_hits.append(
                f"slide {i + 1}: " + ", ".join(f"U+{ord(ch):04X}" for ch in found)
            )
    report.check(
        not emoji_hits,
        "Zero emoji in slide text (Lucide SVG icons only)",
        emoji_hits,
    )

    # (4) Content-slide bullet and word limits.
    limit_violations: list[str] = []
    content_index = 0
    for i, slide in enumerate(slides):
        if slide.kind != "content":
            continue
        content_index += 1
        if slide.bullets > MAX_BULLETS_PER_CONTENT_SLIDE:
            limit_violations.append(
                f"slide {i + 1}: {slide.bullets} bullets "
                f"(max {MAX_BULLETS_PER_CONTENT_SLIDE})"
            )
        if slide.body_words > MAX_BODY_WORDS_PER_CONTENT_SLIDE:
            limit_violations.append(
                f"slide {i + 1}: {slide.body_words} body words "
                f"(max {MAX_BODY_WORDS_PER_CONTENT_SLIDE})"
            )
    report.check(
        not limit_violations,
        f"Content slides within limits "
        f"(<= {MAX_BULLETS_PER_CONTENT_SLIDE} bullets, "
        f"<= {MAX_BODY_WORDS_PER_CONTENT_SLIDE} words)",
        limit_violations,
    )

    # (5) Pinned CDN versions.
    missing_cdns = [cdn for cdn in PINNED_CDNS if cdn not in source]
    report.check(
        not missing_cdns,
        "CDN dependencies pinned to required versions",
        [] if not missing_cdns else [f"missing/incorrect: {missing_cdns}"],
    )

    # (6) Blitzy brand palette tokens.
    upper = source.upper()
    missing_tokens = [tok for tok in PALETTE_TOKENS if tok.upper() not in upper]
    report.check(
        not missing_tokens,
        "Blitzy brand palette tokens present",
        [] if not missing_tokens else [f"missing: {missing_tokens}"],
    )

    # (7) Mermaid / Lucide JavaScript wiring.
    missing_wiring = [snippet for snippet in REQUIRED_WIRING if snippet not in source]
    report.check(
        not missing_wiring,
        "Mermaid/Lucide wiring present "
        "(startOnLoad: false, mermaid.run, lucide.createIcons)",
        [] if not missing_wiring else [f"missing: {missing_wiring}"],
    )

    return report


def main(argv: list[str]) -> int:
    if len(argv) > 1:
        html_path = argv[1]
    else:
        html_path = os.path.join(os.path.dirname(os.path.abspath(__file__)),
                                 "executive-summary.html")

    if not os.path.isfile(html_path):
        print(f"[FAIL] deck file not found: {html_path}", file=sys.stderr)
        return 1

    report = validate(html_path)
    print(f"Validating deck: {html_path}\n")
    print(report.render())
    return 0 if report.passed else 1


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
