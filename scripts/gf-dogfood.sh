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
#   * Preflight: GDOGFOOD_SRC must exist, be a git repository, and hold the
#     working files the checks use (PLAN.md); a clear one-line error otherwise.
#   * Binary: builds/refreshes the release binary from the git-forge checkout
#     that contains this script (never a stale binary) into a controlled temp
#     --target-dir (immune to CARGO_TARGET_DIR / cargo target-dir config), then
#     copies it plus the git-issue/git-pr dispatch wrappers into a private temp
#     bin dir.
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
elif ! git -C "$SRC" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
  echo "gf-dogfood: GDOGFOOD_SRC '$SRC' is not a git repository" >&2; exit 1
elif [ ! -f "$SRC/PLAN.md" ]; then
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
# elsewhere / consume a stale or externally redirected binary.
( cd "$REPO_ROOT" && cargo build --release --target-dir "$TGT" ) >/dev/null
cp "$TGT/release/git-forge" "$BIN/git-forge"
cp "$TGT/release/git-forge" "$BIN/git-issue"
cp "$TGT/release/git-forge" "$BIN/git-pr"

git clone -q "$SRC" "$T/dogfood"
git clone -q "$SRC" "$T/dogfood2"
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
  expected=$(git rev-parse master)
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
git checkout -B feat/dogfood master
printf '\n# dogfood change\n' >> PLAN.md
git add PLAN.md && git commit -qm "feat(dogfood): test change"
git checkout -q master
git tag v1.0 master   # deterministic local tag
ck "pr create snapshot" 0 git forge pr create --source feat/dogfood --base master "dogfood PR"
ck "pr show snapshot fields" 0 git forge pr show 2
ck "pr diff three-dot" 0 git forge pr diff 2
ck "pr create missing source" nonzero git forge pr create --base master T2
ck "pr create missing base" nonzero git forge pr create --source feat/dogfood T2
ck "pr create empty title" nonzero git forge pr create --source feat/dogfood --base master " "
ck "pr create same ref" nonzero git forge pr create --source master --base master T2
ck "pr create tag ref" nonzero git forge pr create --source v1.0 --base master T2
ck "pr create remote-tracking ref" nonzero git forge pr create --source origin/master --base master T2
ck "pr create oid/rev-expr ref" nonzero git forge pr create --source HEAD --base master T2
ck "pr create same-commit distinct refs" nonzero git forge pr create --source master --base master~0 T2
ck "pr ops nonexistent" nonzero git forge pr show 99

# ── FR-005 (fixed): inline review anchor validation ──
FEAT_OID=$(git rev-parse feat/dogfood)
ck "inline no anchor rejected" nonzero git forge pr review 2 --approve --file PLAN.md --line 1
ck "inline bogus anchor rejected" nonzero git forge pr review 2 --approve --file PLAN.md --line 1 --commit deadbeef
ck "inline anchored ok" 0 git forge pr review 2 --approve --file PLAN.md --line 1 --commit "$FEAT_OID"
ck "inline ref-name anchor canonicalizes" 0 git forge pr review 2 --approve --file PLAN.md --line 1 --commit master
at "event stores OID not ref name" 0 event_commit_is_oid
ck "precedence nonexistent PR" nonzero git forge pr review 99 --approve --commit deadbeef

# ── FR-003 / AC-003 / AC-004 / AC-004a/b: gate from NON-base checkout ──
git checkout -q feat/dogfood
ck "approve then reject blocks" 0 git forge pr review 2 --reject
git checkout -q master && git checkout -q feat/dogfood   # ensure non-base
ck "merge blocked after reject" nonzero git forge pr merge 2
ck "reject then approve allows" 0 git forge pr review 2 --approve
git checkout -q feat/dogfood
ck "approved merge succeeds" 0 git forge pr merge 2
ck "pr show merged" 0 git forge pr show 2
at "base contains merged commit" 0 "git merge-base --is-ancestor \$(git rev-parse feat/dogfood) master"
ck "already merged blocked" nonzero git forge pr merge 2

# ── AC-005e / VAL-022: base checked out → refuse (from base itself) ──
git checkout -B feat2 master
printf '\n# change 2\n' >> PLAN.md
git add PLAN.md && git commit -qm "feat(dogfood): change 2"
git checkout -q master
git forge pr create --source feat2 --base master "PR base-checked-out" >/dev/null
git forge pr review 3 --approve >/dev/null
ck "merge refused with base checked out" nonzero git forge pr merge 3
at "checked-out-base error names worktree" 0 "git forge pr merge 3 2>&1 | grep -q 'checked out'"

# ── AC-005b / VAL-017: stale base rejected (from non-base checkout) ──
git checkout -B feat3 master
printf '\n# change 3\n' >> PLAN.md
git add PLAN.md && git commit -qm "feat(dogfood): change 3"
git checkout -q master
git forge pr create --source feat3 --base master "PR stale" >/dev/null
git forge pr review 4 --approve >/dev/null
printf '\n# base advance\n' >> PLAN.md
git add PLAN.md && git commit -qm "base advance after PR"
git checkout -q feat3
ck "stale base merge rejected" nonzero git forge pr merge 4
at "stale error names base_head" 0 "git forge pr merge 4 2>&1 | grep -q 'has moved'"

# ── FR-004 / AC-005 / VAL-005: strategies (fresh PRs, from non-base) ──
git checkout -B feat4 master
printf 'a\n' > f4.txt && git add f4.txt && git commit -qm "f4 c1"
printf 'b\n' >> f4.txt && git add f4.txt && git commit -qm "f4 c2"
git checkout -q master
git forge pr create --source feat4 --base master "squash PR" >/dev/null
git forge pr review 5 --approve >/dev/null
git checkout -q feat4
ck "squash merge ok" 0 git forge pr merge 5 --squash
at "squash = single commit" 0 "[ \$(git rev-list --parents -n 1 master | awk '{print NF}') -eq 2 ] && ! git merge-base --is-ancestor \$(git rev-parse feat4) master"
git checkout -B feat5 master
printf 'x\n' > f5.txt && git add f5.txt && git commit -qm "f5 c1"
printf 'y\n' >> f5.txt && git add f5.txt && git commit -qm "f5 c2"
git checkout -q master
git forge pr create --source feat5 --base master "rebase PR" >/dev/null
git forge pr review 6 --approve >/dev/null
git checkout -q feat5
ck "rebase merge ok" 0 git forge pr merge 6 --rebase
at "rebase = linear history" 0 "[ \$(git rev-list --count --merges master~1..master) -eq 0 ]"

# ── FR-003 / AC-003: truly unapproved PR blocked (fresh #7, from non-base) ──
git checkout -B feat6 master
printf 'z\n' > f6.txt && git add f6.txt && git commit -qm "f6 c1"
git checkout -q master
git forge pr create --source feat6 --base master "unapproved PR" >/dev/null
git checkout -q feat6
ck "unapproved merge blocked (gate)" nonzero git forge pr merge 7

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

echo "=========== DOGFOOD SUMMARY pass=$PASS fail=$FAIL ==========="
[ "$FAIL" -eq 0 ]