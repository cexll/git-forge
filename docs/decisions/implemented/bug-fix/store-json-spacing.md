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
bracketing. Ignore noncanonical suffixes before resolving targets: an Issue ref
uses positive decimal digits, without signs or leading zeros, fitting `u64`.
Propagate unreadable canonical-chain errors with the ref name in list/show.
Issue display rejects chains without a usable creation for the ref's Issue id.
Graph snapshots retain eventless allocation tips for CAS validation.
PR listing retains its existing behavior.

Check counter increments before writing or publishing allocation state.
`allocate_id` and PR creation reject `u64::MAX` with a clean store error.
Issue new/apply preserve their existing `i64::MAX` advanced-counter bound.
An exhausted allocation or batch leaves all refs unchanged.

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
JSON remains readable. Issue lists and graph snapshots fail on unreadable
canonical refs with diagnostic context. Missing and unrelated refs are harmless.
Counter parsing rejects malformed JSON and non-`u64` values. Exhaustion returns
an error rather than panicking or wrapping, and publishes no partial batch.
No migration or source-ref rewrite is needed.
