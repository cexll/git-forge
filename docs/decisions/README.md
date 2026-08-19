# Decision Records (four-zone lifecycle)

Decision records capture the *why* behind non-trivial engineering decisions so
they never have to be re-derived. This tree is the authoritative home for new
decisions; the pre-existing `docs/adr/0001..0006` are legacy history and remain
in place (conventional ADR format, L1 scope already locked).

## Zones

```
docs/decisions/
├── proposed/       # drafts awaiting a decision
├── implemented/    # accepted decisions, cited by later work
├── rejected/       # explicitly declined — kept while they prevent a tempting fallacy
└── archived/       # frozen — never edited
```

Each zone has kind subdirectories: `architecture/`, `process/`, `testing/`,
`feature/`, `bug-fix/`, `simplification/`. A record is classified by what it
decides, not by its size.

## Operational rules

1. **Note-required rule**: a non-trivial change ships its decision record in the
   same change; only mechanical or local edits are exempt.
2. **Supersession at write time**: search active records; fold fully superseded
   records into the new one, cross-link partially superseded ones; a record is
   never edited into a different decision.
3. **Archive freeze**: archived records are frozen. Editing an archived record
   re-classifies the edit as a new proposed note; the frozen original stays.
   Treating the archive as current authority is a category error.
4. **GC by future decision value**: periodically audit; keep any record whose
   alternatives, negative guarantees, ownership boundaries, or reintroduction
   conditions can still steer future work. Delete rejected notes only when they
   no longer prevent a likely mistake. Word count and age are never criteria.
5. **Machine check**: `just decisions-check` runs `scripts/check-decisions.py`,
   which fails on duplicate ids, an edited archived record, a kind outside the
   allowed set, or a record whose header lacks required fields.
6. **One pointer, one home**: `AGENTS.md` carries one pointer line per installed
   rule; the procedure lives here, not copied into AGENTS.md.

## Record format

Each record is a single file: `docs/decisions/<zone>/<kind>/<kebab-case-id>.md`.

```
# <Short Title>

Status: active | superseded by <file> | rejected — <one-line why>

## Problem

<what is being decided, why now>

## Decision            (implemented / proposed)

<the choice and its rationale>

## Alternatives considered

<options weighed, why rejected — required, even if none viable>

## Consequences       (implemented)

<what changed, what to watch>
```

- Lifecycle moves are atomic: moving a record between zones updates the Status
  line and re-satisfies the target skeleton in the same change.
- Implemented records are spec-speak-free: proposal-era headings (`Proposal`,
  `Plan`, `Acceptance criteria`) are folded into Decision/Consequences.
