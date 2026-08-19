# 0006: L1 PRs Are Immutable Snapshots

L1 pull requests are one-shot immutable snapshots: `pr.created` fixes `source_head`, the PR never follows later source-branch movement, and an approval authorizes exactly that snapshot. Proposing new commits means creating a new PR. `pr.update` events and approval invalidation are L2.

Why: without a fixed snapshot, `git forge pr merge` cannot determine which code an approval authorizes after the source branch changes; invalidation semantics would be required in L1. The snapshot rule removes the ambiguity at zero cost for a single-user workflow.

Consequences: re-proposing after review feedback creates a new PR id (noise for multi-round review); that cost is accepted in L1 and revisited with `pr.update` in L2.