# 0003: Command-Level Merge Gate in L1

L1 gates merging inside `git forge pr merge`, requiring an approved Review event before a merge is performed. We do not claim `pre-receive` enforcement for local single-repository merges, because `pre-receive` only runs on the receiving side of a push.

Why: this keeps L1 to a single ordinary repository with the smallest useful workflow, while a centrally enforced branch-protection path (bare receiving remote + `pre-receive`) is deliberately deferred to L2.

Consequences: direct `git merge` bypasses the gate in L1; that boundary is documented, not hidden. Approved design intent is to move enforcement to a bare remote in L2.