# Optional git-forge-workflow companion

The native Rust CLI is unchanged. `scripts/git-forge-workflow` is a Node >=22.16 executable bridge to the updated implement runtime. In the combined delivery layout it finds `../../../skills/implement/scripts/devflow.cjs`; when installed independently set `DEVFLOW_CLI` to the absolute loaded skill entry. Place a symlink to this script on PATH to use `git forge-workflow` (do not overwrite an existing command without checking it).

```
node scripts/git-forge-workflow --help
git forge-workflow publish --spec-dir .specs/job --namespace job-v1 --provider forge --apply
git forge-workflow sync --manifest .specs/job/issue-links.json --repo OWNER/REPO --apply
```

Aliases: publish -> publish-issues; sync -> sync-issues; intake -> intake-issues; submit-pr -> submit-pr; review-pr -> review-pr; reconcile -> reconcile-github; sync-status -> sync-issue-status. All arguments are forwarded as an argv array, never a shell expression; exit status/signal is preserved. This is NOT `git forge issue sync`, NOT a new Rust binary release, and NOT an automatic watcher. No changes to Cargo dependencies or native event schema were made.

The detailed plan/Issue/PR and recovery contract is shipped in the companion implement skill at `references/issue-workflow.md`. Native role attribution (`GIT_FORGE_ROLE`) is a trusted pipeline-seat declaration, not an authenticated separation of human users; GitHub uses an independently authenticated reviewer account. Live acceptance must verify both in the actual host.

Build native CLI with the existing `cargo install --path .` in an environment with its declared Rust toolchain/dependencies. This delivery environment had no Rust toolchain, so native compile/tests were not executed here. The new bridge has its own Node tests; native adapter behavior was exercised through the supplied and newly added test doubles, not represented as a real Rust binary run.
