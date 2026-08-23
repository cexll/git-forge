# On-demand Web browse (L2)

Status: proposed

## Problem

L1 has no web surface; browsing issues/PRs requires the CLI. L2 plans a web UI
("read-only browse vs write paths"). The project's zero-resident-process
constraint means any web server must be transient.

## Decision

- `git forge web` starts a **transient** HTTP server (the `fossil ui` analogue)
  that renders forge state from `refs/forge/*`, and stops when the process ends.
- **Surface**: entity-focused — issue list/show and PR list/show/diff; the
  CLI-equivalent read-only view. No dashboard and no inline review-thread
  rendering in this wave.
- **Stack**: **axum/actix** (user-selected). MIT-licensed, but it pulls a larger
  dependency tree (tokio, etc.), so the dep set must be justified under the repo
  dependency policy and watched against the size gate.
- **Strictly read-only**: all mutations stay on the CLI; the web renderer never
  writes forge refs or creates events.
- **Backend**: fold `refs/forge/*` directly via the store. No Local Index, since
  export/Local Index is deferred out of L2.

## Alternatives considered

- **Hand-rolled std::net minimal HTTP** — rejected: user preferred a full
  framework; hand-rolling HTTP parsing adds framing/encoding footguns.
- **tiny_http** (small MIT crate) — rejected: user chose the full framework for
  standard routing/templating/ecosystem.
- **Local Index (SQLite) query backend** — deferred: export/Local Index is out
  of L2 scope; web folds refs directly.
- **Dashboard / inline CI badges** — deferred: the entity-focused surface was
  chosen for this wave.

## Consequences

- `CONTEXT.md` already has the **On-demand Web** term; unchanged.
- A new web module is added; axum/tokio deps enter `Cargo.toml` (MIT; justify
  and size-gate-watch).
- The web renderer must **HTML-escape** stored content — a distinct concern from
  the terminal `sanitize_terminal`.
- Zero-daemon preserved: the server lives only while the process runs.
