# 0002: Git Extension Surfaces, Not Git Fork

We expose the forge as a git extension: a `git-forge` executable (giving `git forge ...`), optional thin `git-issue`/`git-pr`/`git-ci` wrappers, git hooks, and optional git remote helpers. We do not fork git or embed forge state into git's core.

Why: git's official extension conventions (`git-<cmd>`, `gitremote-helpers`, hooks) already provide the command, transport, and enforcement seams we need, with zero daemon and no fork maintenance cost.

Consequences: `git forge ...` is the single real namespace; short commands are wrappers or aliases. Direct `git merge` bypasses command-level gating in L1 because `pre-receive` only runs on the receiving side of a push.