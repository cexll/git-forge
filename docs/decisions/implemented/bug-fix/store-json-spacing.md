# Read spaced forge JSON without hiding Issue refs

Status: active

## Problem

The counter reader scanned the exact substring `"next":` and consumed only
immediately adjacent digits. A counter written as `{"v": 1, "next": 3}` failed.
Issue listing swallowed that error and scanned no refs, hiding existing Issues.
It also skipped existing eventless Issue refs.

## Decision

Parse the entire counter document with `serde_json` and read `next` as `u64`.
This dependency is confined to the store. The existing std-only event parser
already accepts JSON whitespace and remains the event reader. Neither writer
changes its compact serialization.

Share actual Issue-ref enumeration between graph snapshots and Issue display.
Keep reads through Git ref APIs, numeric ordering, and snapshot counter/CAS
bracketing. Propagate unreadable-chain errors with the ref name. Issue display
rejects chains without a usable creation for the ref's Issue id instead of
silently omitting them. Graph snapshots retain eventless allocation tips for
CAS validation. PR listing retains its existing behavior.

## Alternatives considered

- Reuse the core JSON parser for counters: its signed `i64` representation would
  narrow the existing counter's unsigned `u64` range.
- Widen the core JSON representation or replace the event parser: unnecessary
  public-model and pure-core changes for this counter-read defect.
- Trim whitespace after the substring: still accepts malformed documents and
  does not structurally identify the top-level `next` field.
- Continue scanning ids below the counter: a missing or stale counter can hide
  an existing Issue ref, including an unreadable one.

## Consequences

Older spaced counters work with both `issue new` and `issue apply`. Spaced event
JSON remains readable. Issue lists and graph snapshots fail on unreadable refs
with diagnostic context. Missing refs are still harmless. Counter parsing rejects
malformed JSON and non-`u64` values. No migration or source-ref rewrite is needed.
