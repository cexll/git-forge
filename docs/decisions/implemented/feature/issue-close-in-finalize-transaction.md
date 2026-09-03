# Issue closure is part of the finalize transaction; pre-closed is a no-op

Status: active

## Problem

`finalize_pr_merge` currently atomically deletes the pending result ref and
CAS-updates the base branch and the PR head chain, but nothing closes the linked
Issue. The pipeline contract says `pr create --closes` ends with a closed Issue
(see `forge-pipeline-adapter-contract`, filing clause), so an implementation
that only runs the existing finalizer would leave merged code with an open,
authoritative Issue.

## Decision

When a PR was created with `--closes <issue>`, the issue.close event is written
in the SAME `update-ref --stdin` transaction as the base CAS, the PR-head CAS
and the pending-ref deletion:

- The `issue.close` commit is parented to the issue head; the transaction CASes
  the issue head from its expected tip. On a moved issue head, revalidate and
  retry, mirroring the existing finalize loop.
- The shared counter is NOT involved in closing.
- A target already closed at merge time is a successful no-op — never a second
  close event.
- Finalize retries MUST reuse the single `pr.merge` event UUID and its paired
  `issue.close` UUID — never a second pair of events.

## Alternatives considered

- **Close from the adapter after finalize** — rejected: a crash between the ref
  transaction and the adapter's close leaves merged code with an open Issue;
  the pipeline's crash probe (adapter-contract clause 7) explicitly carries
  "merged but issue still open" as a repair case. The forge-side transaction
  removes that window at the source.

## Consequences

- The transaction grows from three ref moves (pending-ref delete + base CAS +
  PR-head CAS) to four when `--closes` is present.
- The pipeline-side closing comment stays an adapter responsibility (post-merge
  summary), but "issue is closed" becomes a forge transaction guarantee.
- Wire-schema encoding (event body fields, issue-head CAS tuple) lands with
  issue #5's spec.
