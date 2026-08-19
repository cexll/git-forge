# 0001: Git Ref Event Chains as Authoritative Storage

Issues and PRs need durable, git-protocol-synchronizable state without a second protocol. We store each entity as an append-only chain of immutable commits under `refs/forge/*`; current state is derived by folding events. A local SQLite index is a rebuildable cache only, never authoritative.

Why: a normal git repository is already a git-reachable object/ref store, so code and forge state travel through the same push/pull refspecs. SQLite or a single-file backend wins on queries but loses native git interoperability.

Consequences: single mutable ref tips need explicit convergence when forge refs sync is added (L2: remote-tracking namespace + per-entity DAG merge/convergence; L1 is single-repository and has no fetch/merge convergence). Forge refs may be packed and must be read via ref APIs, never filesystem paths.