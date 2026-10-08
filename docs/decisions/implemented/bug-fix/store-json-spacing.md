# Read spaced forge JSON without hiding Issue refs

Status: active

## Problem

The counter reader scanned the exact substring `"next":` and consumed only
immediately adjacent digits. A counter written as `{"v": 1, "next": 3}` failed.
Issue listing swallowed that error and scanned no refs, hiding existing Issues.
It also skipped existing eventless Issue refs.

## Decision

Parse the entire counter document with `serde_json` and require `next` in
`1..=i64::MAX`, matching the event model's supported integer range.
This dependency is confined to the store. The existing std-only event parser
already accepts JSON whitespace and remains the event reader. Neither writer
changes its compact serialization.

Share actual Issue-ref enumeration between graph snapshots and Issue display.
Keep target reads through Git ref APIs, numeric ordering, and snapshot counter/CAS
bracketing. Supplement Git enumeration with immediate loose entries in the common
Git directory: libgit2's reference and name iterators both skip malformed loose
refs. Deduplicate discovered names and resolve canonical names explicitly.
Ignore noncanonical suffixes before resolving targets: an Issue ref uses positive
decimal digits, without signs or leading zeros, fitting `u64`. Propagate lookup
and chain errors with the ref name in list/show, including corrupt loose refs
shadowing packed refs and commands invoked in linked worktrees.
Record matching `issue.created` evidence during each canonical ref's chain read.
Issue display requires this evidence as well as a matching folded id and title;
an `issue.updated` title cannot establish creation. Show renders its requested
Issue from the same validated collection as list, without a second chain fold.
Graph snapshots retain eventless allocation tips for CAS validation and do not
apply display-only creation rejection.
PR listing propagates counter errors instead of treating an invalid counter as
an empty PR range. Its counter-bounded enumeration otherwise remains unchanged.

Check counter increments before writing or publishing allocation state.
All allocation paths reject `i64::MAX` with a clean exhaustion error.
Counters outside the supported range fail with an error naming the counter ref.
An exhausted allocation or batch leaves all refs unchanged.

## Alternatives considered

- Reuse the core JSON parser for counters: avoid changing the pure event model
  for this store-only read and validation boundary.
- Widen the core JSON representation or replace the event parser: unnecessary
  public-model and pure-core changes for this counter-read defect.
- Trim whitespace after the substring: still accepts malformed documents and
  does not structurally identify the top-level `next` field.
- Continue scanning ids below the counter: a missing or stale counter can hide
  an existing Issue ref, including an unreadable one.
- Use libgit2's name iterator alone: it also resolves loose refs and silently
  skips malformed ones. Loose-name discovery is required to report corruption.
- Infer creation from the folded id and title: updates can set both, even after
  a creation for a different Issue id. Only a matching creation event proves it.

## Consequences

Older spaced counters work with both `issue new` and `issue apply`. Spaced event
JSON remains readable. Issue lists and graph snapshots fail on unreadable
canonical refs with diagnostic context. Missing and unrelated refs are harmless.
Counter parsing rejects malformed JSON and values outside `1..=i64::MAX`.
Exhaustion returns an error rather than panicking or wrapping, with no partial batch.
No migration or source-ref rewrite is needed.
