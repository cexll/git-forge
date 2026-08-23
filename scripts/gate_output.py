"""One implementation of the error-message protocol every eng-init gate owes.

`references/gate-quality-contract.md` § Error message protocol publishes a shape:
a title line on stderr, violations indented two spaces, a success summary naming
what was checked, and exit 1 for a violation versus 2 for a usage error. The four
gates each printed something different and none of them matched — the skill was
violating a protocol it wrote for others.

Keeping the format in one module rather than four copies is the same rule the
skill applies to target repos: one concept, one owner. A gate calls `report()`
and gets the contract for free; changing the contract changes every gate at once.
"""
from __future__ import annotations

import sys

VIOLATION = 1
USAGE_ERROR = 2


def report(gate: str, errors: list[str], summary: str) -> int:
    """Print the protocol-shaped result and return the exit code.

    gate    — the gate's name, used as the message prefix so output is greppable.
    errors  — one string per violation; each is indented two spaces under the title.
    summary — what was checked when clean, e.g. "42 files checked, all conform".
              Silent green is forbidden: a CI log must show the gate ran, or a
              gate that stopped running looks exactly like a gate that passed.
    """
    if errors:
        print(f"{gate}: {len(errors)} violation(s) found:", file=sys.stderr)
        for error in errors:
            print(f"  {error}", file=sys.stderr)
        return VIOLATION
    print(f"{gate}: {summary}")
    return 0

# The protocol's third exit code, 2 for a usage error, is delivered by argparse:
# every gate takes its arguments through it and inherits `exit 2` on a bad
# invocation. No helper is exported for it — an entry point with no callers is
# the speculative generality this skill tells target repos to delete.
