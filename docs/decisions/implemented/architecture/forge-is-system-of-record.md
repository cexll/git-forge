# The forge is the system of record; GitHub receives code only

Status: active

## Problem

The repository has two forge-shaped surfaces at once: GitHub (`origin`,
`release.yml`, install script pulling release assets) and git-forge's own
local `refs/forge/*` (issues #1–#9, PRs, Review, CI Check). L1 does not sync
forge refs across clones — `git push` carries code, not forge state — so the
question "where is an Issue authoritative?" has no default answer, and
double-tracking (GitHub Issues + forge Issues) would silently drift.

## Decision

- **git-forge's `refs/forge/*` is the sole system of record** for Issues,
  Pull Requests, Reviews, and CI Checks for this project's agent pipeline.
  Work is filed, gated, and closed there.
- **GitHub is the code-distribution surface**: it receives merged code via
  `git push` of normal branches/tags and hosts release artifacts. Its Issues
  and Pull Requests tabs stay unused (they are already empty — verified
  2026-09-03: zero open issues upstream).
- Forge state travels between machines only when L2 sync lands
  (`forge-sync-single-remote`, proposed); until then a second machine sees code
  but not forge history, and that is accepted, not papered over.

## Alternatives considered

- **GitHub Issues as the work tracker, forge as an adapter** — rejected: two
  counters, two states, no sync; every transition needs a double-write and
  drift becomes the failure mode.
- **Dual-write both surfaces** — rejected: same drift class, strictly more
  machinery. Revival trigger: a second human collaborator who only looks at
  GitHub appears.

## Consequences

- The nine issues filed in this session exist only in this clone's refs;
  losing the clone without a backup of `refs/forge/*` loses them.
  `git bundle`-style ad-hoc backup is the interim answer until L2 sync:
  `git bundle create forge-state.bundle $(git for-each-ref --format='%(refname)' refs/forge)`.
- The existing proposed record `forge-sync-single-remote` (single canonical
  bare remote, pre-receive Merge Gate) is exactly the L2 path this decision
  assumes — no supersession needed; this record pins the interim posture.
- README's "no web UI" sentence gains weight: the human-facing view of
  authority is `git forge issue list` / `pr show`, not a hosted tab.
