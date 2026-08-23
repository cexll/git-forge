# git-forge

## Glossary

### Entities

**Issue** — a tracked request, defect, or discussion item on the repository. Its state is derived by replaying its event chain. _Avoid_: ticket, bug, task.

**Pull Request (PR)** — a proposal to merge a set of commits from a source ref into a base ref, carrying review events and merge state. Its state is derived by replaying its event chain. _Avoid_: merge request.

**Forge Event** — an immutable, appended record in a repository's event chain, written as a git commit. State is derived by folding events in deterministic order. _Avoid_: action, activity.

**Event Chain** — an append-only sequence of Forge Events for one entity, addressed by a namespaced ref. Its tip is a single mutable ref. _Avoid_: log, timeline.

**Review** — a PR-scoped decision event (approve or reject) with optional inline comments. Inline comments anchor a commit hash and do not follow later diff changes. _Avoid_: approval, code review.

### Storage

**Forge Ref** — a git ref under `refs/forge/` that stores entity event chains and related metadata. Ref content may be packed by git; must be discovered via git ref APIs, not filesystem paths. _Avoid_: branch, metadata ref.

**Local Index** — a local, rebuildable cache (initially SQLite) derived from Forge Refs, providing query/search. It is not authoritative. _Avoid_: database, primary store.

**Counter Ref** — the Forge Ref `refs/forge/meta/counter` holding the next sequential issue/PR number. _Avoid_: sequence, id store.

**Canonical Remote** — the single authoritative bare remote holding the forge refs; clones fetch/push forge state to it, and a pre-receive hook enforces the Merge Gate against pushes (planned in L2). _Avoid_: upstream, fork.

### Workflow

**Merge Gate** — the command-level check that blocks `git forge pr merge` until an approved Review event exists and (L2) the latest CI Check is green; direct `git merge` is documented as bypassable in L1. _Avoid_: branch protection.

**CI Run** — an on-demand execution of a configured shell plan whose result is written back as a Forge Event. No server or daemon is involved. _Avoid_: pipeline, job service.

**CI Check** — a Forge Event recording the outcome of a CI Run on a pull request (`status`: pending/success/failed). The Merge Gate requires the latest CI Check to be green (planned in L2). _Avoid_: pipeline, CI gate, job.

**On-demand Web** — a transient web interface (`git forge web`) started only while a user is browsing and stopped when done, analogous to `fossil ui`. _Avoid_: web server, daemon.