# Forge refs sync: single canonical remote (L2)

Status: proposed

## Problem

L1 is single-repository local. Multi-clone/multi-machine sync (declared L2)
needs forge refs to move between clones. A fork-network shape (GitHub-style)
would demand general per-entity DAG convergence, which is more machinery than a
single developer plus an agent pipeline needs.

## Decision

- **Single canonical bare remote** is the L2 sync shape (GitLab-style), not a
  fork network.
- `git forge push` explicitly pushes `refs/forge/*`; `git forge clone/init`
  installs an additive fetch refspec; `git forge pull` converges local forge
  refs to the remote tip.
- The bare remote runs a **pre-receive hook** that enforces the **Merge Gate**
  (approved Review **and** latest CI Check green) for non-base branch pushes.
- devflow consumes by pushing/pulling that one remote and relying on those two.

## Alternatives considered

- **Fork network** (GitHub-style) — rejected: needs per-entity DAG convergence;
  heavier than required for single-user/agent-pipeline consumption.
- **No remote enforcement** — rejected: sync without a gate is advisory only and
  undermines the whole point of moving forge state between clones.

## Consequences

- `CONTEXT.md` gains **Canonical Remote**; the pre-receive hook is a new
  enforcement point separate from the local command-level Merge Gate.
- Refspec wiring and convergence rules are scoped to one remote (see the
  convergence decision record).
- Zero-daemon preserved: push/pull/hook are process-per-invocation.
