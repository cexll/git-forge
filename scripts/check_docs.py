#!/usr/bin/env python3
"""Documentation gate (documentation_gates), wired into `just docs-check`.

Structural drift check over the repo's real documentation:
  1. docs/architecture/git-forge.md must carry a `## Known Limitations`
     section with at least one listed item.
  2. Every backtick-quoted `.md` path referenced from AGENTS.md, CONTEXT.md,
     or the wire contract must resolve to an existing file (catches a renamed
     or removed doc that a stale cross-reference points at).

Content drift stays review-only; this gate catches missing-section and
stale-reference drift. Exit 0 on a clean tree, 1 on a violation, 2 on a usage
error. Uses the shared gate_output protocol so output is greppable and a
silent green is forbidden.
"""
from __future__ import annotations

import re
import sys
from pathlib import Path

from gate_output import report

REPO = Path(__file__).resolve().parent.parent

WIRE = REPO / "docs" / "architecture" / "git-forge.md"
DOC_SOURCES = [REPO / "AGENTS.md", REPO / "CONTEXT.md", WIRE]

KNOWN_LIMITATIONS_RE = re.compile(r"(?ms)^## Known Limitations\s*$\n(?P<body>.*?)(?=^##\s+|\Z)")
BACKTICK = re.compile(r"`([^`]+)`")
PLACEHOLDER_CHARS = set("*{}<>?")
FILE_EXTENSIONS = (".md",)


def looks_like_md_doc(token: str) -> bool:
    """True for a backtick path that names a .md doc we can re-check."""
    if any(c in PLACEHOLDER_CHARS for c in token):
        return False
    if token.endswith("/"):
        return False
    return token.endswith(FILE_EXTENSIONS)


def main() -> int:
    errors: list[str] = []

    if not WIRE.exists():
        errors.append("docs/architecture/git-forge.md missing (required wire contract)")
    else:
        text = WIRE.read_text(encoding="utf-8")
        match = KNOWN_LIMITATIONS_RE.search(text)
        if not match:
            errors.append("docs/architecture/git-forge.md missing `## Known Limitations` section")
        else:
            body = match.group("body").strip()
            listed = any(
                line.lstrip().startswith(("-", "*", "1."))
                for line in body.splitlines()
            )
            if not listed:
                errors.append(
                    "docs/architecture/git-forge.md `## Known Limitations` section has no listed item"
                )

    for source in DOC_SOURCES:
        if not source.exists():
            errors.append(f"{source.relative_to(REPO)} missing (required doc to validate)")
            continue
        for token in BACKTICK.findall(source.read_text(encoding="utf-8")):
            token = token.strip()
            if not looks_like_md_doc(token):
                continue
            path = token.split("#", 1)[0].rstrip(".,;")
            if not (REPO / path).exists():
                errors.append(
                    f"{source.relative_to(REPO)} references missing doc: {path}"
                )

    return report(
        "check-docs",
        errors,
        "check-docs: wire-contract Known Limitations present; doc cross-references resolve",
    )


if __name__ == "__main__":
    sys.exit(main())
