# A forge Review approve is an orchestration gate, not independent human review

Status: active

## Problem

The Merge Gate requires the last reachable Review decision to be `approve`, and
the pipeline drives `git forge pr review --approve` from an orchestrator whose
implementing subagent and reviewing subagent share the repository's single
`user.email` actor. On a single-user machine the gate therefore proves "the
pipeline ran its review step and recorded the decision", not "a person
independent of the author approved" — and the event chain cannot even
distinguish those two states today (see issue #8 for role attribution).

## Decision

Adopt the honest reading and document it:

- In L1 single-user operation, a forge `approve` is an **orchestration
  ceremony** — the machine-readable shadow of the mission's review judgment.
- The authoritative review record stays the devflow finding ledger
  (`record-review` reports with immutable itemIds, rounds, and dispositions);
  the forge Review event is filed when that ledger says pass.
- The wire contract's Known Limitations gains one line so no future reader
  mistakes the gate for human attestation.

## Alternatives considered

- **Claim forge approve as independent review** — rejected: it is false as
  stated; actor identity makes it undecidable, and a verification claim that
  overstates its evidence is the failure mode this repo's own contract-fix
  work exists to prevent.
- **Wait for role attribution (#8) before writing anything** — rejected: the
  note is one line and the misreading risk is live now.

## Consequences

- `docs/architecture/git-forge.md` Known Limitations updated (same change).
- If #8 lands, this note narrows: distinct worker/reviewer actors restore
  gate force for the "not the author" half, never for the "human" half.
- Dogfooders reading `pr show` should treat `effective_decision: approve` as
  "gate passed", per this record.
