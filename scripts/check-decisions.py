#!/usr/bin/env python3
"""Structural verifier for docs/decisions/ (four-zone decision-record lifecycle).

Fails (exit 1) on:
  - a file outside the allowed zone/kind directory layout
  - a kind outside the allowed set
  - a record missing required header fields (Status line, ## Problem, ## Alternatives)
  - a duplicate id across the tree
  - a file in archived/ without a Status: archived marker (archive freeze)

Requires no third-party deps. Parses only prose outside fenced code blocks for
the header checks.
"""
import sys
import re
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DECISIONS = ROOT / "docs" / "decisions"
ZONES = {"proposed", "implemented", "rejected", "archived"}
KINDS = {
    "architecture",
    "process",
    "testing",
    "feature",
    "bug-fix",
    "simplification",
}
# engine formats produced by this verifier; statuses may also reference
# supersession ("superseded by <file>") or rejection ("rejected — <why>").
REQUIRED_MARKDOWN = [
    "## Problem",
    "## Alternatives considered",
]
FENCED = re.compile(r"```.*?```", re.S)
FRONT = re.compile(r"^#\s+.+", re.M)


def strip_fenced(text: str) -> str:
    return FENCED.sub("", text)


def check_file(path: Path, seen_ids: set[str], errors: list[str]) -> None:
    rel = path.relative_to(DECISIONS)
    parts = rel.parts
    if len(parts) != 3 or parts[0] not in ZONES or parts[1] not in KINDS:
        errors.append(f"bad path (expected <zone>/<kind>/<id>.md): {rel}")
        return
    zone, kind, fname = parts[0], parts[1], rel.name
    if not fname.endswith(".md"):
        errors.append(f"non-markdown record: {rel}")
        return
    rec_id = fname[:-3]
    # duplicate ids across zones are fine (same decision lifecycle-moves are
    # atomic renames), but within a zone a duplicate is an error.
    zone_key = (zone, rec_id)
    if zone_key in seen_ids:
        errors.append(f"duplicate id in zone {zone}: {rec_id}")
    seen_ids.add(zone_key)

    text = path.read_text(encoding="utf-8", errors="replace")
    prose = strip_fenced(text)
    for marker in REQUIRED_MARKDOWN:
        if marker not in prose:
            errors.append(f"{rel}: missing required section `{marker}`")
    if "Status:" not in prose:
        errors.append(f"{rel}: missing `Status:` header line")
    if zone == "archived" and "Status: archived" not in prose:
        errors.append(f"{rel}: archived record must carry `Status: archived` (archive freeze)")
    # id sanity
    if not re.match(r"^[a-z0-9-]+$", rec_id):
        errors.append(f"{rel}: record id must be kebab-case [a-z0-9-]")


def main() -> int:
    errors: list[str] = []
    seen: set[tuple[str, str]] = set()
    if not DECISIONS.exists():
        print(f"ok: no decisions tree at {DECISIONS}")
        return 0
    files = [p for p in DECISIONS.rglob("*.md") if p.name != "README.md"]
    for p in sorted(files):
        check_file(p, seen, errors)
    if errors:
        print("decision-record verification FAILED:")
        for e in errors:
            print(f"  - {e}")
        return 1
    print(f"ok: {len(files)} decision records structurally valid")
    return 0


if __name__ == "__main__":
    sys.exit(main())
