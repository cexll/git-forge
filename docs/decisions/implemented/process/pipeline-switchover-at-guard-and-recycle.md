# Pipeline switchover: forge-driven PR flow starts once #3 and #4 land

Status: active

## Problem

The forge cannot host the intended workflow today. `pr merge` refuses while the
base branch is checked out (#3), and it never recycles the worktree that held
the feature branch (#4) — so until both land, the first integration merge of any
PR in this repository is impossible. That creates a bootstrap question: how do
the fixes themselves get in, and when does the pipeline take over?

## Decision

Old path for the enabling changes, then switch immediately:

- Issues #3, #4, #5, #6 and #9 are the enabling set and are
  implemented the way this repository already works: devflow mission, worktree,
  orchestrator `merge --ff-only`. No forge PR is opened for them. #7 (machine
  interface: --json/version) and #8 (actor-role attribution) are code/contract
  issues, not documentation.
- The moment #3, #4, #5 and #9 are all on master, every subsequent issue goes
  through the real flow: `git issue` → worktree → (merge gate) `pr create
  --closes` → `ci run` → `review` → `pr merge --ff --recycle-worktree` — with
  PR creation at merge-gate time (after mission review passes and gate
  verification), not at worker receipt. That includes #1 and #2 — the pipeline
  becomes the vehicle that builds its own remaining prerequisites, and each PR
  closes the issue it implements.
- Fix rounds AND ordinary concurrent waves before #1 land recreate the
  PR — a stale-base refusal is a normal adapter action (`forge_pr_superseded`,
  new snapshot, fresh CI + ceremony approve), not a pause and not
  budget-consuming. They do not block the switch.
- `--forge` is a mission-scoped propose flag. Missions proposed without it keep
  old-path merge semantics to completion; their features never gain a prId and
  no PR is synthesized for pre-switch in_review features.

## Alternatives considered

- **Build everything the old way, flip once at the end** — rejected: big-bang
  cutover puts the adapter's first run against the deep water of #1 and #2 at
  the same time, with no low-risk PRs to calibrate it.
- **Write the devflow adapter against current semantics first** — rejected: the
  adapter would need a code path for recreate-per-round and another for
  `pr.update`, and the first is thrown away when #1 lands.
- **Switch before #3/#4/#5/#9** — impossible, not merely undesirable: the first
  merge is refused and the flag sequence does not exist.

## Consequences

- Between now and the switch, forge state and actual integration are partially
  decoupled: issues exist in the chain, merges do not carry `pr.merge` events.
  Accepted for a bounded window.
- #1 and #2 will be exercised by the flow they unblock, so their acceptance
  criteria should be written in pipeline terms (fix round, concurrent wave),
  not just CLI terms.
- The switchover is one commit boundary and is announced in #4's OLD-PATH
  integration commit message and its Issue comment — there is no pre-switch
  `--recycle-worktree` PR to announce it.
