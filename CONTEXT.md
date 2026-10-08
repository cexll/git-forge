# git-forge

## Glossary

### Entities

**Issue** — a tracked request, defect, or discussion item on the repository. Its state is derived by replaying its event chain. In spec-driven work, a parent Issue represents the spec and child Issues represent authored vertical-slice tickets. _Avoid_: bug, task.

**Issue Key** — an optional stable, repository-wide identity stored on an Issue by declarative apply. Re-applying a key updates the same Issue regardless of the plan's path. Distinct from the sequential Issue id. _Avoid_: filename, session-local key.

**Issue Parent** — the Issue representing a child's spec; `parent` stores its Issue id. Children are derived from parent links, not stored on the parent. Parent links must be acyclic. _Avoid_: blocker, owner.

**Blocking Edge** — a directed `blocked_by` reference from a ticket Issue to an Issue that must close first. An Issue is blocked while any referenced blocker is open; blocking edges must be acyclic and cannot reference the Issue itself. Parent and blocking links are independent. _Avoid_: parent, execution order.

**Issue Plan** — one Markdown file or a nonrecursive, filename-sorted directory of Markdown files with the .md extension describing the desired Issue graph. `issue apply` validates and diffs the complete graph before publishing the plan in one atomic ref transaction. _Avoid_: mission runtime, general YAML.

**Pull Request (PR)** — a proposal to merge a set of commits from a source ref into a base ref, carrying review events and merge state. Its state is derived by replaying its event chain. _Avoid_: merge request.

**Forge Event** — an immutable, appended record in a repository's event chain, written as a git commit. State is derived by folding events in deterministic order. _Avoid_: action, activity.

**Event Chain** — an append-only sequence of Forge Events for one entity, addressed by a namespaced ref. Its tip is a single mutable ref. _Avoid_: log, timeline.

**Review** — a PR-scoped decision event (approve or reject) with optional inline comments. Inline comments anchor a commit hash and do not follow later diff changes. _Avoid_: approval, code review.

### Storage

**Forge Ref** — a git ref under `refs/forge/` that stores entity event chains and related metadata. Ref content may be packed by git; read targets through Git ref APIs. Issue-name discovery also checks loose entries in the common Git directory because libgit2 omits malformed loose refs. _Avoid_: branch, metadata ref.

**Local Index** — a local, rebuildable cache (initially SQLite) derived from Forge Refs, providing query/search. It is not authoritative. _Avoid_: database, primary store.

**Counter Ref** — the Forge Ref `refs/forge/meta/counter` holding the next sequential issue/PR number. _Avoid_: sequence, id store.

**Canonical Remote** — the single authoritative bare remote holding the forge refs; clones fetch/push forge state to it, and a pre-receive hook enforces the Merge Gate against pushes (planned in L2). _Avoid_: upstream, fork.

### Workflow

**Merge Gate** — the command-level check that blocks `git forge pr merge` until an approved Review event exists and the latest CI Check is green (shipped in L1); direct `git merge` is documented as bypassable in L1. _Avoid_: branch protection.

**CI Run** — an on-demand execution of a configured shell plan whose result is written back as a Forge Event. No server or daemon is involved. _Avoid_: pipeline, job service.

**CI Check** — a Forge Event recording the outcome of a CI Run on a pull request (`status`: pending/success/failed). The Merge Gate requires the latest CI Check to be green (shipped in L1). _Avoid_: pipeline, CI gate, job.

**Abandoned PR** — a PR in the terminal state set by a `pr.abandoned` event, carrying the required abandonment `reason`. No mutator opens it again: merge, review, and CI run refuse it, naming the reason, and a second abandon refuses with the original reason; there is no reopen — a reopened PR is a new entity. _Avoid_: cancelled PR, dropped PR, closed PR.

**System of Record** — the surface where an entity's authoritative state lives. For this repository's work tracking, the forge's own `refs/forge/*`; GitHub is a code-distribution mirror only. _Avoid_: tracker, source of truth.

**Spec of Record** — the committed, digest-pinned plan artifacts (spec.md, tickets/, features.json, validation-contract.json) that a mission verifies against. Distinct from **Mission Runtime**. _Avoid_: docs, plan file.

**Mission Runtime** — the scheduler-owned execution state of one run (state, progress log, handoffs, reviews, evidence). Derived, not authoritative, and never committed. _Avoid_: Spec of Record, artifacts.

**Orchestration Ceremony** — a forge event written by the pipeline on a machine where all roles share one identity, evidencing that a step ran rather than that a distinct person judged it. An L1 **Review** approve is one. _Avoid_: human approval, sign-off.

**On-demand Web** — a transient web interface (`git forge web`) started only while a user is browsing and stopped when done, analogous to `fossil ui`. _Avoid_: web server, daemon.