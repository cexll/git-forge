## Issue: t7 — #7 machine interface: JSON output + version probe

Description: `--json` on issue new/list/show, pr create/list/show, ci run (machine-readable result incl. allocated id/status; pr create JSON includes the allocated PR number); `git forge version` prints a semver and exits 0; capabilities discovery DEFERRED (adapter version-gates on semver ranges); unknown flags keep exit-1; the bare `git forge --help` exit-1 quirk stays and is documented.
Type: AFK
Milestone: machine-interface
Touches: src/cli.rs, src/issue.rs, src/pr_merge.rs, src/event.rs, tests/t3_merge.rs, scripts/gf-dogfood.sh, ~/.agents/skills/implement/scripts/devflow.cjs
Blocked by: None
User stories covered: forge-pipeline-p2

### What to build
Per issue #7 spec amendment (1)-(5):

- `--json` on `issue new`, `issue list`, `issue show`, `pr create`, `pr list`, `pr show`, `ci run`: machine-readable result INCLUDING the allocated id/status; `pr create` JSON includes the allocated PR number.
- `git forge version` prints a semver and exits 0.
- Capabilities discovery DEFERRED: the adapter version-gates on semver ranges instead (chosen mechanism).
- Unknown flags keep exit-1; the bare `git forge --help` exit-1 quirk stays and is documented (help lives at `git forge help`).
- Acceptance: one byte-stable schema fixture per JSON-enabled command.

### Acceptance criteria
- [ ] Each JSON-enabled command emits a byte-stable schema fixture (issue new/list/show, pr create/list/show, ci run) matching byte-for-byte
- [ ] `git forge version` prints a semver and exits 0
- [ ] Unknown flags exit-1 (unchanged); bare `git forge --help` exit-1 quirk documented
- [ ] adapter devflow.cjs parses --json (no more sed-parsing prose) and version-gates on semver range

### Validation
Fulfills: VAL-201, VAL-202, VAL-203
- Verification: cargo test (exit zero) + adapter matrix (JSON fixture) (exit zero)
- Command / scenario: each command with --json; schema fixture byte-compare; version output; adapter matrix green with --json
- Evidence expected: schema fixtures byte-identical; version exits 0; adapter matrix (--json path) green

### Notes
- JSON is additive — prose output stays (parallel surface, not replacement).
- The adapter's version-gate replaces flag-probing: `git forge version` semver decides which flags are available.
- The `pr create` JSON MUST carry the allocated PR number (the adapter's id capture).
