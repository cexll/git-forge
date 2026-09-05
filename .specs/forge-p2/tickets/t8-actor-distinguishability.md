## Issue: t8 — #8 actor distinguishability

Description: every Forge Event actor is the repo user.email; in the single-user local setting the orchestrator, the implementing subagent, and the reviewing subagent all share one identity — the Review gate proves 'an approve event exists', not 'someone other than the implementer approved'. The wire contract should say so, and a future multi-agent pipeline wants role attribution in the chain. Identity resolution shells out to `git -C <worktree> config`, so per-worktree identity is already technically reachable via extensions.worktreeConfig or GIT_CONFIG_COUNT/GIT_CONFIG_KEY_0/GIT_CONFIG_VALUE_0 env overrides — but neither is a promised interface.
Type: AFK
Milestone: identity
Touches: src/identity.rs, src/event.rs, src/cli.rs, docs/architecture/git-forge.md, ~/.agents/skills/git-forge/references/wire-contract.md
Blocked by: None
User stories covered: forge-pipeline-p2

### What to build
Per issue #8's amendment (identity coverage):

- A first-class way to attribute events to a role (worker vs reviewer vs orchestrator).
- The wire contract declares the actor field's role semantics.
- Per-worktree identity (extensions.worktreeConfig) and environment overrides (GIT_CONFIG_COUNT/GIT_CONFIG_KEY_0/GIT_CONFIG_VALUE_0) become PROMISED interfaces — documented, tested, stable.
- The Review gate can distinguish 'someone other than the implementer approved'.

### Acceptance criteria
- [ ] Three roles (worker/reviewer/orchestrator) produce distinguishable event actors in one repo
- [ ] Per-worktree identity via extensions.worktreeConfig works and is documented
- [ ] Environment override (GIT_CONFIG_COUNT etc.) works and is documented
- [ ] Wire contract (docs/architecture/git-forge.md + mirror) declares the role semantics
- [ ] Review gate: an approve from the implementer's own role does not satisfy 'someone other than the implementer approved' (when roles are distinct)

### Validation
Fulfills: VAL-204, VAL-209
- Verification: `cargo test` (exit zero)
- Command / scenario: three worktrees with distinct identities; event chain shows distinct actors; gate refuses self-approve
- Evidence expected: event chain actors distinct; gate refusal test green; docs updated

### Notes
- The single-user default stays: when no role is configured, behavior is unchanged (one identity).
- The adapter may pass a role hint per command (orchestrator vs worker) — the CLI accepts it as a promised interface.
