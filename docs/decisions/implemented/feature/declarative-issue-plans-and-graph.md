# Declarative Issue plans and a native spec/ticket graph

Status: active

## Problem

AI planners need to publish a whole spec and its vertical-slice tickets without
partial filing, duplicate Issues, or prose-based identity matching. Executors
need parent links, blocking edges and open/closed state to find the frontier.
The companion integration with the implement/devflow runtime is retired; that
runtime is archived and is no longer a dependency of native forge workflows.

## Decision

Use `git forge issue apply <dir|file> [--dry-run] [--json]` (also `git issue
apply`) to publish declarative Markdown Issue plans. One parent Issue represents
the spec; child Issues represent tickets, with independent `blocked_by` edges.
Stable repository-wide keys identify Issues across re-apply and plan paths.

Use strict, minimal frontmatter rather than a general YAML dependency. Require
`key` and non-empty `title`; support optional parent, inline blocker/label arrays,
and `state: open|closed`. References target plan keys or quoted existing `"#n"`
Issue ids. Read directories nonrecursively in filename order, using regular
`.md` files and ignoring symlinks within directories. Preserve the Markdown body
exactly. Scalars support optional single/double quotes, JSON escapes in double
quotes, and comments at unquoted whitespace followed by `#`.

Omitted labels, parent and blockers clear their fields. Omitted state preserves
an existing Issue's state; new Issues start open. Explicit state enforces it.
Diff entries are create/update/unchanged; JSON is a plain array of
`{key, id, action, changed_fields}`. Dry-run reports tentative ids and writes
nothing. Identical re-apply appends no events and consumes no ids.

Validate the complete desired graph, including existing Issues outside the
plan. Reject duplicate keys, unknown references, self-blocking, blocker cycles
and parent cycles. Share validation with individual `issue new --parent <n>
--blocked-by <n>` operations. Publish every changed Issue chain and any counter
advancement in one ref transaction, verifying every read graph tip and the
counter, with expected absence for newly allocated refs. A dependency or counter
CAS conflict publishes none of the plan; a new attempt needs a fresh snapshot
and full graph validation, not a retry of only changed entries.
Individual `issue new` preserves bounded concurrent allocation retries: capture
and validate a fresh graph after an identified moved-ref CAS conflict, up to the
existing three-attempt limit. Apply itself remains fail-fast on a conflict.

Keep schema v1 additive: `issue.created` gains optional key/parent/blocker fields;
`issue.updated` stores only changed metadata. Open/closed transitions remain
`issue.close` and `issue.reopen`. Old logs fold with absent key/parent and empty
blockers. JSON exposes graph metadata plus computed `blocked`, with derived
children on show. List supports parent and open/closed/all filters, computing
blocking against the complete graph, not only filtered results.

Retire the companion executable, its test and its operational documentation.
Native comment/close/reopen and PR `--closes` remain the execution interfaces;
a successful PR merge still closes its linked Issue atomically. This supersedes
`docs/decisions/implemented/architecture/forge-pipeline-adapter-contract.md` as
an operational integration contract. Its historical rationale remains recorded;
other historical mission decisions, frozen ADRs and archived records are not
rewritten.

## Alternatives considered

- **Keep a companion adapter and mission-local manifests** — rejected: the
  archived runtime would remain an operational dependency and identity would
  still be separate from the authoritative Issue state.
- **File or update Issues one at a time** — rejected: failures leave partial
  specs and unresolved ticket graphs; counter-only atomic allocation is not
  whole-plan atomicity.
- **Match labels or a special body line** — rejected: descriptions must remain
  exact Markdown, and identity must survive moving or reorganizing plan files.
- **General YAML parsing** — rejected: nested configuration and a heavy parser
  add grammar and dependencies that this flat format does not need.
- **CAS only changed Issue refs** — rejected: unchanged dependency targets and
  concurrent allocations can invalidate the validated complete graph.
- **Persist blocked/children** — rejected: these are derived from current open
  blockers and parent links and would become stale when another Issue changes.

## Consequences

Planners publish a whole graph atomically and re-apply safely. Executors read
open, unblocked child Issues to select work and close tickets through the native
PR merge path. Keys are repository-wide identities, so independent specs need
distinct keys rather than directory-scoped names. Omitting metadata deliberately
clears it; omitting state does not reopen completed work.

Whole-graph validation and snapshot verification cost a full Issue graph read;
this is acceptable for L1's local single-repository scope. Prepared unreachable
git objects may remain after a failed transaction, but no partial forge refs or
counter changes are published. No daemon, remote synchronization or runtime
watcher is introduced.

The normative parser, event, display and transaction contract lives in
`docs/architecture/git-forge.md`; README and the repo-local skill describe that
same CLI surface. Main-agent verification must cover the parser, diff,
idempotence, legacy folds, graph display and complete snapshot/CAS conflicts.
