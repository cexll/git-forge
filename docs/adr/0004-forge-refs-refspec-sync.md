# 0004: Forge Refs Sync via Remote-Tracking Namespace and Explicit Convergence (L2)

Status: accepted for L2; not implemented in L1 (single-repository, single-user scope).

`git forge clone` installs an additive fetch refspec mapping forge refs into a remote-tracking namespace — `+refs/forge/*:refs/remotes/<remote>/forge/*` — so ordinary `git fetch`/`git pull` receive remote forge state without touching local authoritative refs. Local `refs/forge/*` are updated only by `git forge pull`, which performs explicit per-entity L2 convergence between the remote-tracking chain and the local chain; the exact algorithm (DAG merge with merge nodes, or a canonical linearization) is deferred to L2 design. Forge refs are pushed by `git forge push` with an explicit refspec; `remote.<name>.push` is never modified.

Why: a force fetch directly into local authoritative refs (`+refs/forge/*:refs/forge/*`) can overwrite an unsynced local issue/PR chain, losing events. Fetching into a remote-tracking namespace keeps remote state observable and makes convergence an explicit, testable operation.

Consequences: ordinary `git push` keeps normal upstream behavior and does not publish forge refs; forge state travels via `git forge push`/`git forge pull`. Required tests: (1) ordinary `git push` still selects its normal upstream branch and does not push forge refs; (2) when local and remote both append to the same entity before a pull, convergence preserves both event sets — neither is lost.