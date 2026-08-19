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

### Workflow

**Merge Gate** — the command-level check that blocks `git forge pr merge` until an approved Review event exists, with direct `git merge` documented as bypassable in L1. _Avoid_: branch protection, CI gate.

**CI Run** — an on-demand execution of a configured shell plan whose result is written back as a Forge Event. No server or daemon is involved. _Avoid_: pipeline, job service.

**On-demand Web** — a transient web interface (`git forge web`) started only while a user is browsing and stopped when done, analogous to `fossil ui`. _Avoid_: web server, daemon.