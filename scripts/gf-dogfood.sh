#!/usr/bin/env bash
# gf-dogfood.sh — git-forge L1 spec dogfood on a disposable clone of a real
# project. Every merge-gate check runs from a NON-base checkout (base-guard
# would otherwise mask the gate). Deterministic branch/tag setup. Fail-fast,
# honest.
#
# Self-contained & deterministic:
#   * Source repo under test: $GDOGFOOD_SRC (default dsh-deepwork). The forge
#     under test is NEVER the live worktree — everything runs against a
#     disposable clone.
#   * Preflight: GDOGFOOD_SRC must exist, resolve to the CANONICAL git worktree
#     root (the top-level reported by git must equal the physical path — a
#     subdirectory that merely contains PLAN.md is rejected), and hold the
#     working files the checks use (PLAN.md); a clear one-line error otherwise.
#   * Binary: builds/refreshes the release binary from the git-forge checkout
#     that contains this script (never a stale binary) into a controlled temp
#     --target-dir (immune to CARGO_TARGET_DIR / cargo target-dir config),
#     with --locked and --offline so the oracle is deterministic and never
#     needs the network; then copies it plus the git-issue/git-pr dispatch
#     wrappers into a private temp bin dir.
#   * Temp dirs: mktemp -d, removed on EXIT via trap. Nothing escapes.
set -eu

# Repo root = the git-forge checkout that contains this script.
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

# Source repo this run dogfoods against (parameterizable).
SRC="${GDOGFOOD_SRC:-/Users/chenwenjie/workspaces/dsh-deepwork}"

# Preflight GDOGFOOD_SRC: an existing git-repo directory holding the working
# files the checks exercise (PLAN.md). Fail loud with a one-line error naming
# the path and the specific requirement.
if [ ! -e "$SRC" ]; then
  echo "gf-dogfood: GDOGFOOD_SRC '$SRC' does not exist" >&2; exit 1
elif [ ! -d "$SRC" ]; then
  echo "gf-dogfood: GDOGFOOD_SRC '$SRC' is not a directory" >&2; exit 1
fi
# The clone source must be the CANONICAL worktree root. `rev-parse
# --is-inside-work-tree` alone would accept a subdirectory, whose clone would
# lack the root layout the checks exercise (F-016). Compute the physical root
# and the git top-level; they must agree exactly.
SRC_CANON="$(cd -P "$SRC" && pwd)" || {
  echo "gf-dogfood: GDOGFOOD_SRC '$SRC' cannot be resolved" >&2; exit 1
}
TOP="$(git -C "$SRC_CANON" rev-parse --show-toplevel 2>/dev/null)" || {
  echo "gf-dogfood: GDOGFOOD_SRC '$SRC' is not a git repository" >&2; exit 1
}
if [ "$SRC_CANON" != "$TOP" ]; then
  echo "gf-dogfood: GDOGFOOD_SRC '$SRC' must be the git worktree root (top-level '$TOP'), not a subdirectory" >&2; exit 1
fi
if [ ! -f "$SRC_CANON/PLAN.md" ]; then
  echo "gf-dogfood: GDOGFOOD_SRC '$SRC' is missing the expected working file PLAN.md" >&2; exit 1
fi

T="$(mktemp -d)"
trap 'rm -rf "$T"' EXIT
BIN="$T/bin"
TGT="$T/target"
mkdir -p "$BIN"

# Build/refresh the release binary from THIS checkout into a controlled temp
# target dir. --target-dir overrides CARGO_TARGET_DIR and cargo target-dir
# config, so the checkout never gains a git-visible target/ and we never build
# elsewhere / consume a stale or externally redirected binary. --locked pins
# the build to the committed Cargo.lock; --offline forbids network access, so
# the oracle cannot silently drift with the crates index (F-017).
( cd "$REPO_ROOT" && cargo build --release --locked --offline --target-dir "$TGT" ) >/dev/null
cp "$TGT/release/git-forge" "$BIN/git-forge"
cp "$TGT/release/git-forge" "$BIN/git-issue"
cp "$TGT/release/git-forge" "$BIN/git-pr"

git clone -q "$SRC_CANON" "$T/dogfood"
git clone -q "$SRC_CANON" "$T/dogfood2"

# Resolve the clone's default branch. `git clone` checks out the source's
# default branch whatever it is named; every checkout/tag/PR below uses
# $BASE_BRANCH, never a hardcoded branch name (F-033). An empty resolution
# (detached/unborn HEAD) fails loudly and names the source — never guess.
BASE_BRANCH="$(git -C "$T/dogfood" branch --show-current)"
if [ -z "$BASE_BRANCH" ]; then
  echo "gf-dogfood: GDOGFOOD_SRC '$SRC' has no checked-out default branch (branch --show-current empty); refusing to guess" >&2
  exit 1
fi

export PATH="$BIN:$PATH"
PASS=0; FAIL=0
ck() { # ck <name> <expected_exit 0|nonzero> <cmd...>
  local name="$1" want="$2"; shift 2
  set +e; "$@" >"$T/gf-df.out" 2>"$T/gf-df.err"; local got=$?; set -e
  if { [ "$want" = 0 ] && [ "$got" = 0 ]; } || { [ "$want" = nonzero ] && [ "$got" != 0 ]; }; then
    PASS=$((PASS+1)); echo "PASS $name"
  else
    FAIL=$((FAIL+1)); echo "FAIL $name (exit=$got want=$want)"; sed 's/^/    /' "$T/gf-df.err" | head -3
  fi
}
at() { # at <name> <expected 0|nonzero> <shell test cmd>
  local name="$1" want="$2"; shift 2
  set +e; if eval "$*"; then local got=0; else local got=1; fi; set -e
  if [ "$got" = "$want" ]; then PASS=$((PASS+1)); echo "PASS $name";
  else FAIL=$((FAIL+1)); echo "FAIL $name (test exit=$got want=$want)"; fi
}
event_commit_is_oid() { # event stores OID, not ref name
  local expected actual
  expected=$(git rev-parse "$BASE_BRANCH")
  actual=$(git show "$(git rev-parse refs/forge/prs/2/head):.forge/event.json")
  [[ "$actual" == *"\"commit\":\"$expected\""* ]]
}

R="$T/dogfood"
cd "$R"
git config user.name Dogfood && git config user.email dogfood@x

# ── FR-001 / AC-001 / AC-007 / VAL-014: issue lifecycle + lazy counter ──
ck "issue new #1" 0 git forge issue new "dogfood bug"
ck "issue list shows #1" 0 git forge issue list
ck "issue show #1" 0 git forge issue show 1
ck "issue comment" 0 git forge issue comment 1 "repro noted"
ck "issue close" 0 git forge issue close 1
ck "issue reopen" 0 git forge issue reopen 1
ck "issue invalid id clean error" nonzero git forge issue show abc
ck "issue empty title rejected" nonzero git forge issue new "   "
ck "issue comment on missing" nonzero git forge issue comment 99 x

# ── FR-002 / AC-002 / AC-002a..e / AC-002i / VAL-021/028: PR create guards ──
git checkout -B feat/dogfood "$BASE_BRANCH"
printf '\n# dogfood change\n' >> PLAN.md
# Refuse to create the plan through a tracked `.forge`/`.forge/ci.sh` symlink:
# a source repo that tracks either as a symlink would make this write follow
# the link and mutate a path outside the disposable clone `$T/dogfood`.
if [ -L .forge ] || [ -L .forge/ci.sh ]; then
  echo "gf-dogfood: refusing to write .forge/ci.sh: .forge or .forge/ci.sh is a symlink (tracked escape)" >&2
  exit 1
fi
mkdir -p .forge && printf '#!/bin/sh\necho ok\n' > .forge/ci.sh   # green CI plan (t3)
git add PLAN.md .forge/ci.sh && git commit -qm "feat(dogfood): test change"
git checkout -q "$BASE_BRANCH"
git tag v1.0 "$BASE_BRANCH"   # deterministic local tag
ck "pr create snapshot" 0 git forge pr create --source feat/dogfood --base "$BASE_BRANCH" "dogfood PR"
ck "pr show snapshot fields" 0 git forge pr show 2
ck "pr diff three-dot" 0 git forge pr diff 2
ck "pr create missing source" nonzero git forge pr create --base "$BASE_BRANCH" T2
ck "pr create missing base" nonzero git forge pr create --source feat/dogfood T2
ck "pr create empty title" nonzero git forge pr create --source feat/dogfood --base "$BASE_BRANCH" " "
ck "pr create same ref" nonzero git forge pr create --source "$BASE_BRANCH" --base "$BASE_BRANCH" T2
ck "pr create tag ref" nonzero git forge pr create --source v1.0 --base "$BASE_BRANCH" T2
ck "pr create remote-tracking ref" nonzero git forge pr create --source "origin/$BASE_BRANCH" --base "$BASE_BRANCH" T2
ck "pr create oid/rev-expr ref" nonzero git forge pr create --source HEAD --base "$BASE_BRANCH" T2
ck "pr create same-commit distinct refs" nonzero git forge pr create --source "$BASE_BRANCH" --base "$BASE_BRANCH~0" T2
ck "pr ops nonexistent" nonzero git forge pr show 99

# ── FR-005 (fixed): inline review anchor validation ──
FEAT_OID=$(git rev-parse feat/dogfood)
ck "inline no anchor rejected" nonzero git forge pr review 2 --approve --file PLAN.md --line 1
ck "inline bogus anchor rejected" nonzero git forge pr review 2 --approve --file PLAN.md --line 1 --commit deadbeef
ck "inline anchored ok" 0 git forge pr review 2 --approve --file PLAN.md --line 1 --commit "$FEAT_OID"
ck "inline ref-name anchor canonicalizes" 0 git forge pr review 2 --approve --file PLAN.md --line 1 --commit "$BASE_BRANCH"
at "event stores OID not ref name" 0 event_commit_is_oid
ck "precedence nonexistent PR" nonzero git forge pr review 99 --approve --commit deadbeef

# ── FR-003 / AC-003 / AC-004 / AC-004a/b: gate from NON-base checkout ──
git checkout -q feat/dogfood
ck "approve then reject blocks" 0 git forge pr review 2 --reject
git checkout -q "$BASE_BRANCH" && git checkout -q feat/dogfood   # ensure non-base
ck "merge blocked after reject" nonzero git forge pr merge 2
ck "reject then approve allows" 0 git forge pr review 2 --approve
git forge ci run 2 >/dev/null   # green latest CI Check so the gate lets #2 merge (t3)
git checkout -q feat/dogfood
ck "approved merge succeeds" 0 git forge pr merge 2
ck "pr show merged" 0 git forge pr show 2
at "base contains merged commit" 0 "git merge-base --is-ancestor \$(git rev-parse feat/dogfood) \"\$BASE_BRANCH\""
ck "already merged blocked" nonzero git forge pr merge 2

# ── #3 / VAL-105/106: live-base refresh — a CLEAN checked-out base merges and
# the base worktree is refreshed to the result commit; a DIRTY base (tracked
# modification OR untracked file) still refuses with refs unchanged; ignored
# files do NOT count as dirty ──
git checkout -B feat2 "$BASE_BRANCH"
printf '\n# change 2\n' >> PLAN.md
mkdir -p .forge && printf '#!/bin/sh\necho ok\n' > .forge/ci.sh   # green CI plan (t3)
git add PLAN.md .forge/ci.sh && git commit -qm "feat(dogfood): change 2"
git checkout -q "$BASE_BRANCH"
printf 'scratch.log\n' >> .gitignore   # tracked: makes the ignored-only case real
git add .gitignore && git commit -qm "chore(dogfood): ignore scratch log"
git forge pr create --source feat2 --base "$BASE_BRANCH" "PR base-checked-out" >/dev/null
git forge pr review 3 --approve >/dev/null
git forge ci run 3 >/dev/null   # green latest CI Check so the gate lets #3 merge (t3)
at "dirty tracked base refused" 0 "printf '\\n# dirty tracked\\n' >> PLAN.md && ! git forge pr merge 3 2>/dev/null"
at "dirty refusal names the dirt" 0 "git forge pr merge 3 2>&1 | grep -q 'dirty'"
at "dirty refusal leaves base ref" 0 "[ \"\$(git rev-parse \"\$BASE_BRANCH\")\" = \"\$(git rev-parse refs/forge/prs/3/base)\" ]"
at "dirty refusal leaves PR unmerged" 0 "! git forge pr show 3 | grep -q 'merged:'"
at "untracked file counts as dirty" 0 "printf 'u\\n' > gf-untracked-dirty.txt && ! git forge pr merge 3 2>/dev/null; rm gf-untracked-dirty.txt"
at "untracked refusal leaves base ref" 0 "[ \"\$(git rev-parse \"\$BASE_BRANCH\")\" = \"\$(git rev-parse refs/forge/prs/3/base)\" ]"
git checkout -q -- PLAN.md   # restore the tracked modification before the later cases
printf 'log\\n' > scratch.log   # ignored-only: must NOT block
at "ignored-only is not dirty" 0 "[ -z \"\$(git status --porcelain)\" ]"
ck "clean checked-out base merges" 0 git forge pr merge 3
at "base worktree refreshed to result" 0 "[ \"\$(git rev-parse HEAD)\" = \"\$(git rev-parse \"\$BASE_BRANCH\")\" ] && git merge-base --is-ancestor feat2 \"\$BASE_BRANCH\" && [ -z \"\$(git status --porcelain)\" ]"
at "ignored file survives refresh" 0 "[ -f scratch.log ]"
rm scratch.log

# ── AC-005b / VAL-017: stale base rejected (from non-base checkout) ──
git checkout -B feat3 "$BASE_BRANCH"
printf '\n# change 3\n' >> PLAN.md
mkdir -p .forge && printf '#!/bin/sh\necho ok\n' > .forge/ci.sh   # green CI plan (t3)
git add PLAN.md .forge/ci.sh && git commit -qm "feat(dogfood): change 3"
git checkout -q "$BASE_BRANCH"
git forge pr create --source feat3 --base "$BASE_BRANCH" "PR stale" >/dev/null
git forge pr review 4 --approve >/dev/null
git forge ci run 4 >/dev/null   # green latest CI Check so the gate lets #4 reach the stale-base guard (t3)
printf '\n# base advance\n' >> PLAN.md
git add PLAN.md && git commit -qm "base advance after PR"
git checkout -q feat3
ck "stale base merge rejected" nonzero git forge pr merge 4
at "stale error names base_head" 0 "git forge pr merge 4 2>&1 | grep -q 'has moved'"

# ── FR-004 / AC-005 / VAL-005: strategies (fresh PRs, from non-base) ──
git checkout -B feat4 "$BASE_BRANCH"
printf 'a\n' > f4.txt && git add f4.txt && git commit -qm "f4 c1"
printf 'b\n' >> f4.txt
mkdir -p .forge && printf '#!/bin/sh\necho ok\n' > .forge/ci.sh   # green CI plan (t3)
git add f4.txt .forge/ci.sh && git commit -qm "f4 c2"
git checkout -q "$BASE_BRANCH"
git forge pr create --source feat4 --base "$BASE_BRANCH" "squash PR" >/dev/null
git forge pr review 5 --approve >/dev/null
git forge ci run 5 >/dev/null   # green latest CI Check so the gate lets #5 merge (t3)
git checkout -q feat4
ck "squash merge ok" 0 git forge pr merge 5 --squash
at "squash = single commit" 0 "[ \$(git rev-list --parents -n 1 \"\$BASE_BRANCH\" | awk '{print NF}') -eq 2 ] && ! git merge-base --is-ancestor \$(git rev-parse feat4) \"\$BASE_BRANCH\""
git checkout -B feat5 "$BASE_BRANCH"
printf 'x\n' > f5.txt && git add f5.txt && git commit -qm "f5 c1"
printf 'y\n' >> f5.txt
mkdir -p .forge && printf '#!/bin/sh\necho ok\n' > .forge/ci.sh   # green CI plan (t3)
git add f5.txt .forge/ci.sh && git commit -qm "f5 c2"
git checkout -q "$BASE_BRANCH"
git forge pr create --source feat5 --base "$BASE_BRANCH" "rebase PR" >/dev/null
git forge pr review 6 --approve >/dev/null
git forge ci run 6 >/dev/null   # green latest CI Check so the gate lets #6 merge (t3)
git checkout -q feat5
ck "rebase merge ok" 0 git forge pr merge 6 --rebase
at "rebase = linear history" 0 "[ \$(git rev-list --count --merges \"\$BASE_BRANCH~1..\$BASE_BRANCH\") -eq 0 ]"

# ── FR-003 / AC-003: truly unapproved PR blocked (fresh #7, from non-base) ──
git checkout -B feat6 "$BASE_BRANCH"
printf 'z\n' > f6.txt && git add f6.txt && git commit -qm "f6 c1"
git checkout -q "$BASE_BRANCH"
git forge pr create --source feat6 --base "$BASE_BRANCH" "unapproved PR" >/dev/null
git checkout -q feat6
ck "unapproved merge blocked (gate)" nonzero git forge pr merge 7

# ── s0 / VAL-001: pr create --closes records + shows the relationship ──
git checkout -B feat/gf-closes "$BASE_BRANCH"
printf 'q\n' > gf-closes.txt && git add gf-closes.txt && git commit -qm "closes c1"
git checkout -q "$BASE_BRANCH"
ck "pr create with closes" 0 git forge pr create --source feat/gf-closes --base "$BASE_BRANCH" "PR closes" --closes 1
at "pr show renders closes" 0 "git forge pr show 8 | grep -q 'closes: #1'"
at "pr.created event carries closes id" 0 "git show refs/forge/prs/8/meta:.forge/event.json | grep -q '\"closes\":1'"
at "no closes line without --closes" 0 "! git forge pr show 2 | grep -q 'closes:'"
ck "pr create nonexistent closes" nonzero git forge pr create --source feat/gf-closes --base "$BASE_BRANCH" T-closes --closes 99
at "rejected closes consumes no sequence id" 0 "git forge pr create --source feat/gf-closes --base \"\$BASE_BRANCH\" after-bad >/dev/null && git forge pr show 9 | grep -q 'after-bad'"
# ── s1 / VAL-002/003: merge with --closes closes the linked issue in the SAME
# finalize transaction; an already-closed target is a no-op (no second close) ──
git forge pr review 8 --approve >/dev/null
git forge ci run 8 >/dev/null
git checkout -q feat/gf-closes
ck "merge with closes succeeds" 0 git forge pr merge 8
at "merge closes linked issue" 0 "git forge issue show 1 | grep -q closed"
at "two close events on issue chain" 0 "[ \$(git log --format='%s' refs/forge/issues/1 | grep -c 'forge:issue.close:1') -eq 2 ]"
git checkout -B feat/gf-closes2 "$BASE_BRANCH"
printf 'r\n' > gf-closes2.txt && git add gf-closes2.txt && git commit -qm "closes2 c1"
git checkout -q "$BASE_BRANCH"
git forge pr create --source feat/gf-closes2 --base "$BASE_BRANCH" "closes PR 2" --closes 1 >/dev/null
git forge pr review 10 --approve >/dev/null
git forge ci run 10 >/dev/null
git checkout -q feat/gf-closes2
ck "pre-closed target merge succeeds" 0 git forge pr merge 10
at "no second close event" 0 "[ \$(git log --format='%s' refs/forge/issues/1 | grep -c 'forge:issue.close:1') -eq 2 ]"
at "issue still closed after pre-closed merge" 0 "git forge issue show 1 | grep -q closed"
# ── s2 / VAL-004: a failed merge keeps the linked issue open (t3) ──
git forge issue new "gate red target" >/dev/null
git checkout -B feat/gf-gatered "$BASE_BRANCH"
printf 'g\n' > gf-gatered.txt && git add gf-gatered.txt && git commit -qm "gatered"
git checkout -q "$BASE_BRANCH"
git forge pr create --source feat/gf-gatered --base "$BASE_BRANCH" "gate red closes" --closes 11 >/dev/null
git checkout -q feat/gf-gatered
ck "gate-red closes merge blocked" nonzero git forge pr merge 12
at "gate red keeps linked issue open" 0 "git forge issue show 11 | grep -q open"

git forge issue new "stale target" >/dev/null
git checkout -B feat/gf-stale "$BASE_BRANCH"
printf 's\n' > gf-stale.txt
mkdir -p .forge && printf '#!/bin/sh\necho ok\n' > .forge/ci.sh   # green CI plan (t3)
git add gf-stale.txt .forge/ci.sh && git commit -qm "stale"
git checkout -q "$BASE_BRANCH"
git forge pr create --source feat/gf-stale --base "$BASE_BRANCH" "stale closes" --closes 13 >/dev/null
git forge pr review 14 --approve >/dev/null
git forge ci run 14 >/dev/null
printf '\n# t3 base advance\n' >> PLAN.md
git add PLAN.md && git commit -qm "t3 base advance"
git checkout -q feat/gf-stale
ck "stale-base closes merge blocked" nonzero git forge pr merge 14
at "stale base keeps linked issue open" 0 "git forge issue show 13 | grep -q open"

git forge issue new "conflict target" >/dev/null
git checkout -q "$BASE_BRANCH"
printf 'v1\n' > gf-shared.txt && git add gf-shared.txt && git commit -qm "conflict base v1"
git checkout -B feat/gf-conflict "$BASE_BRANCH"
printf 'v2\n' > gf-shared.txt
mkdir -p .forge && printf '#!/bin/sh\necho ok\n' > .forge/ci.sh   # green CI plan (t3)
git add gf-shared.txt .forge/ci.sh && git commit -qm "conflict feature v2"
git checkout -q "$BASE_BRANCH"
printf 'v3\n' > gf-shared.txt && git add gf-shared.txt && git commit -qm "conflict base v3"
git forge pr create --source feat/gf-conflict --base "$BASE_BRANCH" "conflict closes" --closes 15 >/dev/null
git forge pr review 16 --approve >/dev/null
git forge ci run 16 >/dev/null
git checkout -q feat/gf-conflict
ck "conflict closes merge blocked" nonzero git forge pr merge 16
at "conflict keeps linked issue open" 0 "git forge issue show 15 | grep -q open"

# ── VAL-005/006 / t4: pr abandon terminal state ──
git checkout -B feat/gf-abandon "$BASE_BRANCH"
printf 'w\n' > gf-abandon.txt && git add gf-abandon.txt && git commit -qm "abandon c1"
git checkout -q "$BASE_BRANCH"
git forge pr create --source feat/gf-abandon --base "$BASE_BRANCH" "abandon PR" >/dev/null
ck "abandon missing reason rejected" nonzero git forge pr abandon 17
ck "abandon empty reason rejected" nonzero git forge pr abandon 17 --reason "   "
ck "pr abandon with reason" 0 git forge pr abandon 17 --reason "not needed anymore"
at "pr show renders abandoned reason" 0 "git forge pr show 17 | grep -q 'abandoned: not needed anymore'"
ck "merge refuses abandoned PR" nonzero git forge pr merge 17
at "merge error names abandon reason" 0 "git forge pr merge 17 2>&1 | grep -q 'not needed anymore'"
ck "second abandon refused" nonzero git forge pr abandon 17 --reason again
at "single pr.abandoned event on chain" 0 "[ \$(git log --format='%s' refs/forge/prs/17/head | grep -c 'forge:pr.abandoned:17') -eq 1 ]"
at "second abandon error names original reason" 0 "git forge pr abandon 17 --reason again 2>&1 | grep -q 'not needed anymore'"
at "show keeps original reason after second abandon" 0 "git forge pr show 17 | grep -q 'abandoned: not needed anymore' && ! git forge pr show 17 | grep -q 'abandoned: again'"
ck "abandon merged PR refused" nonzero git forge pr abandon 2 --reason late
at "merged PR abandon appends no event" 0 "[ \$(git log --format='%s' refs/forge/prs/2/head | grep -c 'forge:pr.abandoned:2') -eq 0 ]"
ck "abandon unknown PR refused" nonzero git forge pr abandon 99 --reason x
# ── VAL-007/008 / t5: review and ci run refuse an abandoned PR ──
ck "review refuses abandoned PR" nonzero git forge pr review 17 --approve
at "review error names abandon reason" 0 "git forge pr review 17 --approve 2>&1 | grep -q 'not needed anymore'"
at "abandoned PR still unreviewed after refusal" 0 "git forge pr show 17 | grep -q 'no review yet'"
ck "ci run refuses abandoned PR" nonzero git forge ci run 17
at "ci run error names abandon reason" 0 "git forge ci run 17 2>&1 | grep -q 'not needed anymore'"
at "abandoned PR CI state still pending after refusal" 0 "git forge pr show 17 | grep -q 'ci: pending'"
at "no new ci.check after refused ci run" 0 "[ \$(git log --format='%s' refs/forge/prs/17/head | grep -c 'forge:ci.check:17') -eq 1 ]"

# ── AC-006a / VAL-015: concurrent comment CAS on an independent clone ──
R2="$T/dogfood2"
cd "$R2"
git config user.name Dogfood && git config user.email dogfood@x
git forge issue new "concurrent" >/dev/null
set +e
git forge issue comment 1 "c1" >/dev/null 2>&1 & P1=$!
git forge issue comment 1 "c2" >/dev/null 2>&1 & P2=$!
wait $P1; E1=$?
wait $P2; E2=$?
set -e
at "both concurrent comments exit 0" 0 "[ \$E1 -eq 0 ] && [ \$E2 -eq 0 ]"
at "both comments folded in show" 0 "git forge issue show 1 | grep -q c1 && git forge issue show 1 | grep -q c2"

# ── #9 / VAL-101..102: pr merge --ff fast-forward strategy ──
cd "$R"
git checkout -q -B feat/gf-ff "$BASE_BRANCH"
printf 'n\n' > gf-ff.txt && git add gf-ff.txt && git commit -qm "ff c1"
git checkout -q "$BASE_BRANCH"
git forge pr create --source feat/gf-ff --base "$BASE_BRANCH" "ff PR" >/dev/null
git forge pr review 18 --approve >/dev/null
git forge ci run 18 >/dev/null   # green latest CI Check so the gate lets #18 merge (t3)
BASE_TIP="$(git rev-parse "$BASE_BRANCH")"
SRC_HEAD="$(git rev-parse feat/gf-ff)"
git checkout -q feat/gf-ff
ck "ff merge ok" 0 git forge pr merge 18 --ff
at "ff result_commit is source_head" 0 "git forge pr show 18 | grep -q \"merged: \$SRC_HEAD\""
at "ff base tip is source head" 0 "[ \"\$(git rev-parse \"\$BASE_BRANCH\")\" = \"\$SRC_HEAD\" ]"
at "ff no new commit single parent" 0 "[ \$(git rev-list --parents -n 1 \"\$BASE_BRANCH\" | awk '{print NF}') -eq 2 ] && [ \"\$(git rev-list --parents -n 1 \"\$BASE_BRANCH\" | awk '{print \$2}')\" = \"\$BASE_TIP\" ]"
at "ff pr show merged" 0 "git forge pr show 18 | grep -q 'merged:'"

# ── #9 / VAL-102: non-ffable PR refuses --ff naming the rebase remedy ──
git checkout -q -b feat/gf-ff-noff "$BASE_BRANCH"
printf 'p\n' > gf-ff-noff.txt && git add gf-ff-noff.txt && git commit -qm "ff noff c1"
git checkout -q "$BASE_BRANCH"
printf '\n# base advance for noff\n' >> PLAN.md
git add PLAN.md && git commit -qm "base advance for noff"
git forge pr create --source feat/gf-ff-noff --base "$BASE_BRANCH" "noff PR" >/dev/null
git forge pr review 19 --approve >/dev/null
git forge ci run 19 >/dev/null   # green latest CI Check so the gate lets #19 reach the ff check (t3)
NOFF_BASE="$(git rev-parse "$BASE_BRANCH")"
git checkout -q feat/gf-ff-noff
ck "non-ff merge rejected" nonzero git forge pr merge 19 --ff
at "noff error names rebase remedy" 0 "git forge pr merge 19 --ff 2>&1 | grep -qE 'rebase|recreate'"
at "noff base unchanged" 0 "[ \"\$(git rev-parse \"\$BASE_BRANCH\")\" = \"\$NOFF_BASE\" ]"
at "noff pr still unmerged" 0 "! git forge pr show 19 | grep -q 'merged:'"

# ── #4 / VAL-109..112: pr merge --recycle-worktree — outcome vocabulary ──
# Worktrees live OUTSIDE the disposable clone (a worktree inside it would
# register as untracked dirt and trip the checked-out base guard). $WR is
# trap-removed with $T; the at() strings double-escape \$ (eval expands
# twice), matching the established escaping.
WR="$T/gf-wt"
mkdir -p "$WR"
git worktree add -q "$WR/src" -b feat/gf-recycle "$BASE_BRANCH"
printf 'r\n' > "$WR/src/gf-recycle.txt"
git -C "$WR/src" add gf-recycle.txt && git -C "$WR/src" commit -qm "recycle c1"
git forge pr create --source feat/gf-recycle --base "$BASE_BRANCH" "recycle PR" >/dev/null
git forge pr review 20 --approve >/dev/null
git forge ci run 20 >/dev/null   # green latest CI Check so the gate lets #20 merge (t3)
# refused-not-contained: an UNREGISTERED path refuses, PR unmerged.
at "recycle unregistered path refused" 0 "git forge pr merge 20 --ff --recycle-worktree \$WR/nope 2>&1 | tee \"\$T/rc.out\" | grep -q refused-not-contained"
at "recycle unregistered refusal unmerged" 0 "! git forge pr show 20 | grep -q 'merged:'"
# refused-not-contained: a REGISTERED worktree on the WRONG branch refuses
# and the referenced worktree is untouched.
git worktree add -q "$WR/wrong" -b feat/gf-wrong "$BASE_BRANCH"
at "recycle wrong-branch worktree refused" 0 "git forge pr merge 20 --ff --recycle-worktree \$WR/wrong 2>&1 | grep -q refused-not-contained"
at "recycle wrong worktree untouched" 0 "[ -d \$WR/wrong ] && git worktree list --porcelain | grep -qF \"\$WR/wrong\""
# refused-dirty: a dirty source worktree (untracked file) refuses without
# --force; worktree and branch stay intact, PR unmerged.
printf 'u\n' > "$WR/src/gf-untracked.txt"
at "recycle dirty refused" 0 "git forge pr merge 20 --ff --recycle-worktree \$WR/src 2>&1 | tee \"\$T/rc.out\" | grep -q refused-dirty"
at "recycle dirty keeps worktree and branch" 0 "[ -d \$WR/src ] && git rev-parse -q --verify feat/gf-recycle >/dev/null"
at "recycle dirty keeps PR unmerged" 0 "! git forge pr show 20 | grep -q 'merged:'"
# --force proceeds: outcome removed + branch-deleted, worktree gone and
# unregistered, branch rev-parse fails (VAL-109 evidence).
at "recycle force merge ok with outcomes" 0 "git forge pr merge 20 --ff --force --recycle-worktree \$WR/src 2>&1 | tee \"\$T/rc.out\" | grep -q removed && grep -q branch-deleted \"\$T/rc.out\""
at "recycle force worktree gone" 0 "[ ! -d \$WR/src ] && ! git worktree list --porcelain | grep -qF \"\$WR/src\""
at "recycle force branch deleted" 0 "! git rev-parse -q --verify feat/gf-recycle >/dev/null"
git worktree remove --force "$WR/wrong"

# ── #4 / VAL-111: branch-kept — squash merge with recycle keeps the branch
# and reports branch-kept(<reason>) naming why ──
git worktree add -q "$WR/keep" -b feat/gf-keep "$BASE_BRANCH"
printf 'k\n' > "$WR/keep/gf-keep.txt"
git -C "$WR/keep" add gf-keep.txt && git -C "$WR/keep" commit -qm "keep c1"
git forge pr create --source feat/gf-keep --base "$BASE_BRANCH" "keep PR" >/dev/null
git forge pr review 21 --approve >/dev/null
git forge ci run 21 >/dev/null   # green latest CI Check so the gate lets #21 merge (t3)
at "squash recycle merge ok with branch-kept" 0 "git forge pr merge 21 --squash --recycle-worktree \$WR/keep 2>&1 | tee \"\$T/rc.out\" | grep -q removed && grep -qF 'branch-kept(' \"\$T/rc.out\""
at "squash recycle worktree removed" 0 "[ ! -d \$WR/keep ] && ! git worktree list --porcelain | grep -qF \"\$WR/keep\""
at "squash recycle branch still exists" 0 "git rev-parse -q --verify feat/gf-keep >/dev/null"

# ── t1 / VAL-205/206: pr.update — stale approve refused, fresh approve+CI merges ──
git checkout -q -B feat/gf-update "$BASE_BRANCH"
printf 'u1\n' > gf-update.txt && git add gf-update.txt && git commit -qm "update c1"
git checkout -q "$BASE_BRANCH"
git forge pr create --source feat/gf-update --base "$BASE_BRANCH" "update PR" >/dev/null
git forge pr review 22 --approve >/dev/null
git forge ci run 22 >/dev/null   # green latest CI Check so the gate reaches the stale check (t3)
git checkout -q feat/gf-update
printf 'u2\n' >> gf-update.txt && git add gf-update.txt && git commit -qm "update fix round"
U2="$(git rev-parse feat/gf-update)"
git checkout -q feat/gf-update
at "update re-pins as generation 1" 0 "git forge pr update 22 | grep -q 'generation 1'"
at "generation ref pins the new tip" 0 "[ \"\$(git rev-parse refs/forge/prs/22/gen/1/source)\" = \"\$U2\" ]"
at "generation-0 source ref immutable" 0 "[ \"\$(git rev-parse refs/forge/prs/22/source)\" != \"\$U2\" ]"
at "pr.update event records new source head" 0 "git show refs/forge/prs/22/head:.forge/event.json | grep -qF '\"new_source_head\":\"'\$U2"
at "pr.update event records old base head" 0 "git show refs/forge/prs/22/head:.forge/event.json | grep -q 'old_base_head'"
at "show exposes generation 1" 0 "git forge pr show 22 | grep -q 'generation: 1'"
at "stale approve merge refused" 0 "! git forge pr merge 22 2>/dev/null"
at "stale refusal names the gate" 0 "git forge pr merge 22 2>&1 | grep -q 'stale'"
git forge ci run 22 >/dev/null
git forge pr review 22 --approve >/dev/null
git checkout -q feat/gf-update
at "fresh approve+CI merge passes" 0 "git forge pr merge 22"
at "merged base contains post-update commit" 0 "git merge-base --is-ancestor \$U2 \"\$BASE_BRANCH\""

# ── t2 / #2 live-base merge: two PRs on one base; the second's base moves —
# refuse without fresh gate, pass with fresh CI + fresh approve bound to the
# moved base tip; the result contains the concurrent base advance ──
git checkout -q -B feat/gf-lb1 "$BASE_BRANCH"
printf 'l1\n' > gf-lb1.txt && git add gf-lb1.txt && git commit -qm "lb1 c1"
git checkout -q "$BASE_BRANCH"
git forge pr create --source feat/gf-lb1 --base "$BASE_BRANCH" "lb first PR" >/dev/null
git forge pr review 23 --approve >/dev/null
git forge ci run 23 >/dev/null
git checkout -q -B feat/gf-lb2 "$BASE_BRANCH"
printf 'l2\n' > gf-lb2.txt && git add gf-lb2.txt && git commit -qm "lb2 c1"
git checkout -q "$BASE_BRANCH"
git forge pr create --source feat/gf-lb2 --base "$BASE_BRANCH" "lb second PR" >/dev/null
git forge pr review 24 --approve >/dev/null
git forge ci run 24 >/dev/null
# Move the base via PR #23's merge (unmoved legacy path) so PR #24's base has
# genuinely advanced; LB_MOVED is the moved tip the refusals must preserve and
# the merged result must contain.
git forge pr merge 23 >/dev/null
LB_MOVED="$(git rev-parse "$BASE_BRANCH")"
at "lb second merge refused without prepared candidate" 0 "! git forge pr merge 24 2>/dev/null"
at "lb refusal names prepare" 0 "git forge pr merge 24 2>&1 | grep -q 'prepare'"
at "lb refusal leaves base ref" 0 "[ \"\$(git rev-parse \"\$BASE_BRANCH\")\" = \"\$LB_MOVED\" ]"
at "lb refusal leaves PR unmerged" 0 "! git forge pr show 24 | grep -q 'merged:'"
ck "lb prepare candidate" 0 git forge pr prepare 24 --strategy merge
at "lb pr show renders prepared line" 0 "git forge pr show 24 | grep -q 'prepared: strategy merge result'"
# Candidate CI runs BEFORE the refusal: the Check is bound to the prepared
# candidate while the approve is still unbound, so the refusal is genuinely
# the CI-alone case and names the approve remedy.
git forge ci run 24 >/dev/null
at "lb candidate CI alone still refused" 0 "! git forge pr merge 24 2>/dev/null"
at "lb CI-alone refusal names approve" 0 "git forge pr merge 24 2>&1 | grep -q 'approve'"
git forge pr review 24 --approve >/dev/null
ck "lb live-base merge passes with prepared candidate" 0 git forge pr merge 24
at "lb result contains the moved base" 0 "git merge-base --is-ancestor \$LB_MOVED \"\$BASE_BRANCH\""
at "lb result contains the second PR source" 0 "git merge-base --is-ancestor feat/gf-lb2 \"\$BASE_BRANCH\""
at "lb pr show merged" 0 "git forge pr show 24 | grep -q 'merged:'"

echo "=========== DOGFOOD SUMMARY pass=$PASS fail=$FAIL ==========="
[ "$FAIL" -eq 0 ]