//! t8 identity integration tests (#8 actor distinguishability): role
//! attribution via `GIT_FORGE_ROLE`, the promised identity-override
//! interfaces (extensions.worktreeConfig, GIT_CONFIG_COUNT env), the
//! self-approval merge gate, and the unchanged single-user default. Runs the
//! built `git-forge` binary as a subprocess inside an isolated temp git
//! repository (same harness shape as t1b/t2/t3).

use std::path::{Path, PathBuf};
use std::process::Command;

fn tmpdir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "gf-t4-{tag}-{}-{:?}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .subsec_nanos()
    ));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn git(dir: &Path, args: &[&str]) -> (i32, String, String) {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .output()
        .unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).trim().to_string(),
        String::from_utf8_lossy(&out.stderr).trim().to_string(),
    )
}

fn init_repo(dir: &Path) {
    let (c, _, e) = git(dir, &["init", "-q"]);
    assert!(c == 0, "git init failed: {e}");
    // Local identity only — no user-level config can leak in (hermetic forge).
    let (c, _, e) = git(dir, &["config", "user.name", "Test"]);
    assert!(c == 0, "config user.name failed: {e}");
    let (c, _, e) = git(dir, &["config", "user.email", "worker@example.com"]);
    assert!(c == 0, "config user.email failed: {e}");
    std::fs::write(dir.join("base.txt"), "base\n").unwrap();
    let (c, _, e) = git(dir, &["add", "base.txt"]);
    assert!(c == 0, "git add failed: {e}");
    let (c, _, e) = git(dir, &["commit", "-q", "-m", "base commit"]);
    assert!(c == 0, "base commit failed: {e}");
    let (c, _, e) = git(dir, &["branch", "-M", "main"]);
    assert!(c == 0, "branch -M failed: {e}");
}

/// Run the git-forge binary in `dir` under a HERMETIC env: global/system
/// config disabled, ambient `GIT_CONFIG_*`/`GIT_FORGE_ROLE` removed, so an
/// identity injected by an outer harness (forge CI) can never mask an
/// assertion. `envs` adds exactly the overrides the test wants.
fn forge_with_env(dir: &Path, args: &[&str], envs: &[(&str, &str)]) -> (i32, String, String) {
    let bin = env!("CARGO_BIN_EXE_git-forge");
    let clean = tmpdir("hermetic");
    let mut cmd = Command::new(bin);
    cmd.args(args)
        .current_dir(dir)
        .env("HOME", &clean)
        .env("XDG_CONFIG_HOME", &clean)
        .env("XDG_GLOBAL_CONFIG", &clean)
        .env("GIT_CONFIG_NOSYSTEM", "1");
    for k in [
        "GIT_CONFIG_GLOBAL",
        "GIT_CONFIG_SYSTEM",
        "GIT_CONFIG_COUNT",
        "GIT_CONFIG_KEY_0",
        "GIT_CONFIG_VALUE_0",
        "GIT_FORGE_ROLE",
    ] {
        cmd.env_remove(k);
    }
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let out = cmd.output().unwrap();
    let _ = std::fs::remove_dir_all(&clean);
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).trim().to_string(),
        String::from_utf8_lossy(&out.stderr).trim().to_string(),
    )
}

fn forge(dir: &Path, args: &[&str]) -> (i32, String, String) {
    forge_with_env(dir, args, &[])
}

fn ref_oid(dir: &Path, r: &str) -> Option<String> {
    let (c, o, _) = git(dir, &["rev-parse", "--verify", r]);
    if c == 0 {
        Some(o)
    } else {
        None
    }
}

/// Make a feature branch off main with one commit touching `file`.
fn make_feature_file(dir: &Path, branch: &str, file: &str, content: &str) {
    git(dir, &["checkout", "-q", "-b", branch]);
    std::fs::write(dir.join(file), content).unwrap();
    git(dir, &["add", file]);
    git(dir, &["commit", "-q", "-m", &format!("{branch} commit")]);
    git(dir, &["checkout", "-q", "main"]);
}

/// Every event JSON on the entity chain (oldest→tip), read from the raw wire
/// blobs (`.forge/event.json` of each event-bearing commit; genesis and
/// non-event commits are skipped).
fn chain_events(dir: &Path, r: &str) -> Vec<String> {
    let (c, out, _) = git(dir, &["rev-list", r]);
    if c != 0 {
        return Vec::new();
    }
    let mut evs = Vec::new();
    for oid in out.lines() {
        let (c2, json, _) = git(dir, &["show", &format!("{oid}:.forge/event.json")]);
        if c2 == 0 {
            evs.push(json);
        }
    }
    evs.reverse();
    evs
}

/// The `role` body value of an event JSON, when present.
fn event_role(event_json: &str) -> Option<String> {
    let idx = event_json.find("\"role\":\"")?;
    let rest = &event_json[idx + "\"role\":\"".len()..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// Make a feature branch off main with one commit (hermetic git env).
fn make_feature(dir: &Path, branch: &str, content: &str) {
    git(dir, &["checkout", "-q", "-b", branch]);
    std::fs::write(dir.join("feature.txt"), content).unwrap();
    git(dir, &["add", "feature.txt"]);
    git(dir, &["commit", "-q", "-m", &format!("{branch} commit")]);
    git(dir, &["checkout", "-q", "main"]);
}

/// A green `.forge/ci.sh` committed on the source branch.
fn commit_ci_plan(dir: &Path) {
    std::fs::create_dir_all(dir.join(".forge")).unwrap();
    std::fs::write(dir.join(".forge").join("ci.sh"), "#!/bin/sh\nexit 0\n").unwrap();
    git(dir, &["add", ".forge/ci.sh"]);
    git(dir, &["commit", "-q", "-m", "ci plan"]);
}

// ── Role attribution on events ──────────────────────────────────────────────

#[test]
fn role_env_stamps_role_on_issue_events() {
    let dir = tmpdir("role-issue");
    init_repo(&dir);
    let (c, _, e) = forge_with_env(
        &dir,
        &["forge", "issue", "new", "t8 title"],
        &[("GIT_FORGE_ROLE", "worker")],
    );
    assert_eq!(c, 0, "issue new failed: {e}");
    let (c, _, e) = forge_with_env(
        &dir,
        &["forge", "issue", "comment", "1", "hello"],
        &[("GIT_FORGE_ROLE", "reviewer")],
    );
    assert_eq!(c, 0, "issue comment failed: {e}");
    let (c, _, e) = forge_with_env(
        &dir,
        &["forge", "issue", "close", "1"],
        &[("GIT_FORGE_ROLE", "orchestrator")],
    );
    assert_eq!(c, 0, "issue close failed: {e}");

    let evs = chain_events(&dir, "refs/forge/issues/1");
    assert_eq!(evs.len(), 3, "issue chain events");
    assert_eq!(
        event_role(&evs[0]).as_deref(),
        Some("worker"),
        "created: {}",
        evs[0]
    );
    assert_eq!(
        event_role(&evs[1]).as_deref(),
        Some("reviewer"),
        "comment: {}",
        evs[1]
    );
    assert_eq!(
        event_role(&evs[2]).as_deref(),
        Some("orchestrator"),
        "close: {}",
        evs[2]
    );
    // The actor stays the email; role is additive provenance, never part of
    // the actor value (F-028 actor-is-email-only is untouched).
    assert!(
        evs[0].contains("\"actor\":\"worker@example.com\""),
        "actor must remain the email: {}",
        evs[0]
    );
}

#[test]
fn role_invalid_value_refuses_cleanly_without_writing() {
    let dir = tmpdir("role-invalid");
    init_repo(&dir);
    let (c, _, e) = forge_with_env(
        &dir,
        &["forge", "issue", "new", "t"],
        &[("GIT_FORGE_ROLE", "wrker")],
    );
    assert_ne!(c, 0, "invalid role must fail");
    assert!(
        e.contains("GIT_FORGE_ROLE"),
        "stderr must name the role problem: {e}"
    );
    // Fail-closed: no event, no counter, no ref — nothing written.
    assert!(ref_oid(&dir, "refs/forge/issues/1").is_none());
    assert!(ref_oid(&dir, "refs/forge/meta/counter").is_none());
}

#[test]
fn role_env_empty_is_no_role() {
    let dir = tmpdir("role-empty");
    init_repo(&dir);
    let (c, _, e) = forge_with_env(
        &dir,
        &["forge", "issue", "new", "t"],
        &[("GIT_FORGE_ROLE", "")],
    );
    assert_eq!(c, 0, "empty role env must mean no role: {e}");
    let evs = chain_events(&dir, "refs/forge/issues/1");
    assert_eq!(evs.len(), 1);
    assert!(
        !evs[0].contains("\"role\""),
        "no role key expected on the wire: {}",
        evs[0]
    );
}

// ── PR chain roles + show/JSON rendering ────────────────────────────────────

#[test]
fn pr_create_json_creator_role_matches_show() {
    let dir = tmpdir("create-role-render");
    init_repo(&dir);
    make_feature(&dir, "feature", "feat\n");

    for role in [None, Some("worker")] {
        let envs: Vec<_> = role
            .map(|value| ("GIT_FORGE_ROLE", value))
            .into_iter()
            .collect();
        let (c, created, e) = forge_with_env(
            &dir,
            &[
                "forge", "pr", "create", "--source", "feature", "--base", "main", "role PR",
                "--json",
            ],
            &envs,
        );
        assert_eq!(c, 0, "create --json: {e}");
        let created: serde_json::Value = serde_json::from_str(&created).unwrap();
        let expected = role.map_or(serde_json::Value::Null, |value| value.into());
        assert_eq!(
            created.get("creator_role"),
            Some(&expected),
            "create --json must return the recorded creator role"
        );

        let id = created["id"].as_u64().unwrap().to_string();
        let (c, shown, e) = forge(&dir, &["forge", "pr", "show", &id, "--json"]);
        assert_eq!(c, 0, "show --json: {e}");
        let shown: serde_json::Value = serde_json::from_str(&shown).unwrap();
        assert_eq!(
            shown.get("creator_role"),
            created.get("creator_role"),
            "create and show must agree on the recorded creator role"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn three_roles_produce_distinguishable_pr_actors_in_one_repo() {
    // THE VAL-204 scenario: worker creates, reviewer approves, orchestrator
    // merges — one repo, ONE shared user.email, three distinguishable event
    // actors via per-command GIT_FORGE_ROLE. The identity is shared on
    // PURPOSE: role attribution must distinguish actors even when the
    // underlying user.email is identical (the single-machine pipeline case).
    let dir = tmpdir("role-pr-chain");
    init_repo(&dir);
    make_feature(&dir, "feature", "feat\n");
    commit_ci_plan(&dir);
    make_feature(&dir, "feature2", "feat2\n");
    let (c, _, e) = forge_with_env(
        &dir,
        &[
            "forge", "pr", "create", "--source", "feature2", "--base", "main", "role PR",
        ],
        &[("GIT_FORGE_ROLE", "worker")],
    );
    assert_eq!(c, 0, "pr create failed: {e}");
    let (c, _, e) = forge_with_env(
        &dir,
        &["forge", "pr", "review", "1", "--approve"],
        &[("GIT_FORGE_ROLE", "reviewer")],
    );
    assert_eq!(c, 0, "pr review failed: {e}");
    let (c, _, e) = forge_with_env(
        &dir,
        &["forge", "ci", "run", "1"],
        &[("GIT_FORGE_ROLE", "orchestrator")],
    );
    assert_eq!(c, 0, "ci run failed: {e}");

    let evs = chain_events(&dir, "refs/forge/prs/1/head");
    assert_eq!(
        event_role(&evs[0]).as_deref(),
        Some("worker"),
        "created: {}",
        evs[0]
    );
    let review = evs
        .iter()
        .find(|j| j.contains("\"pr.review\""))
        .expect("review event on chain");
    assert_eq!(
        event_role(review).as_deref(),
        Some("reviewer"),
        "review: {review}"
    );
    let ci = evs
        .iter()
        .rev()
        .find(|j| j.contains("\"ci.check\"") && j.contains("\"plan\""))
        .expect("ci.check event on chain");
    assert_eq!(event_role(ci).as_deref(), Some("orchestrator"), "ci: {ci}");
    // All actors share the repo email — the ROLE is the distinguishing
    // provenance on the chain.
    for j in [evs[0].as_str(), review.as_str(), ci.as_str()] {
        assert!(
            j.contains("\"actor\":\"worker@example.com\""),
            "actor must remain the shared email: {j}"
        );
    }
}

#[test]
fn pr_show_and_json_render_roles_when_configured() {
    let dir = tmpdir("role-render");
    init_repo(&dir);
    make_feature(&dir, "feature", "feat\n");
    commit_ci_plan(&dir);
    make_feature(&dir, "feature2", "feat2\n");
    let (c, _, e) = forge_with_env(
        &dir,
        &[
            "forge",
            "pr",
            "create",
            "--source",
            "feature2",
            "--base",
            "main",
            "render PR",
        ],
        &[("GIT_FORGE_ROLE", "worker")],
    );
    assert_eq!(c, 0, "create: {e}");
    let (c, _, e) = forge_with_env(
        &dir,
        &["forge", "pr", "review", "1", "--approve"],
        &[("GIT_FORGE_ROLE", "reviewer")],
    );
    assert_eq!(c, 0, "review: {e}");

    let (c, o, e) = forge(&dir, &["forge", "pr", "show", "1"]);
    assert_eq!(c, 0, "show: {e}");
    assert!(
        o.contains("created-by: worker"),
        "show must render creator role: {o}"
    );
    assert!(
        o.contains("decision-by: reviewer"),
        "show must render decision role: {o}"
    );

    let (c, o, e) = forge(&dir, &["forge", "pr", "show", "1", "--json"]);
    assert_eq!(c, 0, "show --json: {e}");
    assert!(
        o.contains("\"creator_role\":\"worker\""),
        "json creator_role: {o}"
    );
    assert!(
        o.contains("\"decision_role\":\"reviewer\""),
        "json decision_role: {o}"
    );

    // A roleless chain (the single-user default) renders neither.
    let (c, _, e) = forge(
        &dir,
        &[
            "forge", "pr", "create", "--source", "feature", "--base", "main", "plain PR",
        ],
    );
    assert_eq!(c, 0, "plain create: {e}");
    let (c, o, e) = forge(&dir, &["forge", "pr", "show", "2"]);
    assert_eq!(c, 0, "plain show: {e}");
    assert!(!o.contains("created-by:"), "no role line expected: {o}");
    let (c, o, _) = forge(&dir, &["forge", "pr", "show", "2", "--json"]);
    assert_eq!(c, 0);
    assert!(
        o.contains("\"creator_role\":null") && o.contains("\"decision_role\":null"),
        "roleless json must fold null roles: {o}"
    );
}

// ── Self-approval merge gate (#8) ───────────────────────────────────────────

#[test]
fn merge_gate_refuses_self_role_approve_and_accepts_distinct_role() {
    let dir = tmpdir("self-approve");
    init_repo(&dir);
    make_feature(&dir, "feature", "feat\n");
    commit_ci_plan(&dir);
    make_feature(&dir, "feature2", "feat2\n");
    // Worker creates the PR...
    let (c, _, e) = forge_with_env(
        &dir,
        &[
            "forge", "pr", "create", "--source", "feature2", "--base", "main", "self PR",
        ],
        &[("GIT_FORGE_ROLE", "worker")],
    );
    assert_eq!(c, 0, "create: {e}");
    // ...and approves its OWN work under the SAME role: the approve is
    // recorded (the chain is an honest log) but must NOT satisfy the gate.
    let (c, _, e) = forge_with_env(
        &dir,
        &["forge", "pr", "review", "1", "--approve"],
        &[("GIT_FORGE_ROLE", "worker")],
    );
    assert_eq!(c, 0, "self approve records: {e}");
    let (c, _, e) = forge(&dir, &["forge", "ci", "run", "1"]);
    assert_eq!(c, 0, "ci run: {e}");
    let base_before = ref_oid(&dir, "refs/heads/main").unwrap();
    let (c, _, e) = forge(&dir, &["forge", "pr", "merge", "1"]);
    assert_ne!(c, 0, "self-approval must refuse the merge");
    assert!(
        e.contains("self-approv"),
        "refusal must name self-approval: {e}"
    );
    assert!(e.contains("worker"), "refusal must name the role: {e}");
    // Refusal leaves the base untouched (gate refuses before any side effect).
    assert_eq!(
        ref_oid(&dir, "refs/heads/main").unwrap(),
        base_before,
        "refused merge must not move the base"
    );
    // A DIFFERENT role's approve satisfies the gate (last review wins).
    let (c, _, e) = forge_with_env(
        &dir,
        &["forge", "pr", "review", "1", "--approve"],
        &[("GIT_FORGE_ROLE", "reviewer")],
    );
    assert_eq!(c, 0, "reviewer approve: {e}");
    let (c, o, e) = forge(&dir, &["forge", "pr", "merge", "1"]);
    assert_eq!(c, 0, "distinct-role merge must pass: {e} / {o}");
}

#[test]
fn merge_gate_unchanged_without_roles() {
    // Single-user default: no roles anywhere → the gate is exactly the
    // pre-t8 approve+CI gate (the orchestration ceremony is unchanged).
    let dir = tmpdir("no-role-default");
    init_repo(&dir);
    make_feature(&dir, "feature", "feat\n");
    commit_ci_plan(&dir);
    make_feature(&dir, "feature2", "feat2\n");
    let (c, _, e) = forge(
        &dir,
        &[
            "forge", "pr", "create", "--source", "feature2", "--base", "main", "plain PR",
        ],
    );
    assert_eq!(c, 0, "create: {e}");
    let (c, _, e) = forge(&dir, &["forge", "pr", "review", "1", "--approve"]);
    assert_eq!(c, 0, "approve: {e}");
    let (c, _, e) = forge(&dir, &["forge", "ci", "run", "1"]);
    assert_eq!(c, 0, "ci: {e}");
    let (c, _, e) = forge(&dir, &["forge", "pr", "merge", "1"]);
    assert_eq!(c, 0, "roleless merge must pass unchanged: {e}");
    let evs = chain_events(&dir, "refs/forge/prs/1/head");
    assert!(
        evs.iter().all(|j| !j.contains("\"role\"")),
        "roleless chain must carry no role keys"
    );
}

#[test]
fn merge_gate_open_when_only_one_side_carries_a_role() {
    // Documented refusal policy: the comparison runs only when BOTH the
    // pr.created and the effective approve record roles. One-sided role
    // coverage (legacy chains, partially-configured pipelines) keeps the
    // gate open — indistinguishable is not self-approval. Distinct files per
    // branch: feature2's PR merges first, so main carries feature2.txt; a
    // later PR from `feature` must not content-conflict with it.
    let dir = tmpdir("one-sided-role");
    init_repo(&dir);
    make_feature_file(&dir, "feature", "feature.txt", "feat\n");
    commit_ci_plan(&dir);
    make_feature_file(&dir, "feature2", "feature2.txt", "feat2\n");
    // a) roleless create + role=worker approve → allowed.
    let (c, _, e) = forge(
        &dir,
        &[
            "forge",
            "pr",
            "create",
            "--source",
            "feature2",
            "--base",
            "main",
            "one-sided A",
        ],
    );
    assert_eq!(c, 0, "create A: {e}");
    let (c, _, e) = forge_with_env(
        &dir,
        &["forge", "pr", "review", "1", "--approve"],
        &[("GIT_FORGE_ROLE", "worker")],
    );
    assert_eq!(c, 0, "approve A: {e}");
    let (c, _, e) = forge(&dir, &["forge", "ci", "run", "1"]);
    assert_eq!(c, 0, "ci A: {e}");
    let (c, _, e) = forge(&dir, &["forge", "pr", "merge", "1"]);
    assert_eq!(c, 0, "one-sided approve must not refuse: {e}");
    // b) role=worker create + roleless approve → allowed. (`feature` needs
    // its own green plan first: its snapshot has none yet.)
    git(&dir, &["checkout", "-q", "feature"]);
    commit_ci_plan(&dir);
    git(&dir, &["checkout", "-q", "main"]);
    let (c, _, e) = forge_with_env(
        &dir,
        &[
            "forge",
            "pr",
            "create",
            "--source",
            "feature",
            "--base",
            "main",
            "one-sided B",
        ],
        &[("GIT_FORGE_ROLE", "worker")],
    );
    assert_eq!(c, 0, "create B: {e}");
    let (c, _, e) = forge(&dir, &["forge", "pr", "review", "2", "--approve"]);
    assert_eq!(c, 0, "approve B: {e}");
    let (c, _, e) = forge(&dir, &["forge", "ci", "run", "2"]);
    assert_eq!(c, 0, "ci B: {e}");
    let (c, o, e) = forge(&dir, &["forge", "pr", "merge", "2"]);
    assert_eq!(
        c,
        0,
        "one-sided create role must not refuse; stdout={o} stderr={e} log={}",
        git(&dir, &["log", "--oneline", "--graph", "--all"]).1
    );
}

// ── Promised identity-override interfaces (#8) ──────────────────────────────

#[test]
fn worktree_config_identity_is_a_promised_interface() {
    // extensions.worktreeConfig + `git config --worktree user.email`:
    // identity resolution shells out to `git config`, so a per-worktree
    // identity MUST be honored — the documented contract, now promised.
    let dir = tmpdir("wt-config");
    init_repo(&dir);
    let (c, _, e) = git(&dir, &["config", "extensions.worktreeConfig", "true"]);
    assert_eq!(c, 0, "enable worktreeConfig: {e}");
    let (c, _, e) = git(
        &dir,
        &["config", "--worktree", "user.email", "wt@example.com"],
    );
    assert_eq!(c, 0, "set worktree user.email: {e}");
    let (c, o, e) = forge(&dir, &["forge", "issue", "new", "wt issue"]);
    assert_eq!(c, 0, "issue new: {e}");
    let evs = chain_events(&dir, "refs/forge/issues/1");
    assert!(
        evs[0].contains("\"actor\":\"wt@example.com\""),
        "actor must be the per-worktree email: {}",
        evs[0]
    );
    let _ = o;
}

#[test]
fn git_config_count_env_override_is_a_promised_interface() {
    // GIT_CONFIG_COUNT/GIT_CONFIG_KEY_n/GIT_CONFIG_VALUE_n override the
    // identity for one command without touching any config file — the
    // documented env contract, now promised.
    let dir = tmpdir("cfg-count");
    init_repo(&dir);
    let (c, _, e) = forge_with_env(
        &dir,
        &["forge", "issue", "new", "env issue"],
        &[
            ("GIT_CONFIG_COUNT", "1"),
            ("GIT_CONFIG_KEY_0", "user.email"),
            ("GIT_CONFIG_VALUE_0", "env@example.com"),
        ],
    );
    assert_eq!(c, 0, "issue new under env override: {e}");
    let evs = chain_events(&dir, "refs/forge/issues/1");
    assert!(
        evs[0].contains("\"actor\":\"env@example.com\""),
        "actor must be the env-override email: {}",
        evs[0]
    );
    let (_, out, _) = git(&dir, &["config", "--local", "--get", "user.email"]);
    assert_eq!(
        out, "worker@example.com",
        "local config must stay untouched"
    );
}

#[test]
fn three_worktrees_distinct_identities_distinct_actors() {
    // The ticket's scenario: three worktrees of ONE repo, each configured
    // with a distinct user.email via extensions.worktreeConfig, produce
    // three distinguishable event actors in the same forge — without any
    // role and without touching the shared repo config.
    let dir = tmpdir("three-wt");
    init_repo(&dir);
    let (c, _, e) = git(&dir, &["config", "extensions.worktreeConfig", "true"]);
    assert_eq!(c, 0, "enable worktreeConfig: {e}");
    let (c, _, e) = git(&dir, &["commit", "-q", "-m", "empty", "--allow-empty"]);
    assert_eq!(c, 0, "empty commit: {e}");
    let wt_root = dir.parent().unwrap();
    let mut actors = Vec::new();
    for (i, (wt, email)) in [
        ("wt-worker", "worker@wt"),
        ("wt-review", "reviewer@wt"),
        ("wt-orch", "orchestrator@wt"),
    ]
    .iter()
    .enumerate()
    {
        let path = wt_root.join(format!(
            "{wt}-{i}-{}",
            dir.file_name().unwrap().to_string_lossy()
        ));
        let main_oid = ref_oid(&dir, "refs/heads/main").unwrap();
        let (c, _, e) = git(
            &dir,
            &[
                "worktree",
                "add",
                "-q",
                "--detach",
                path.to_str().unwrap(),
                &main_oid,
            ],
        );
        assert_eq!(c, 0, "worktree add: {e}");
        let (c, _, e) = git(&path, &["config", "--worktree", "user.email", email]);
        assert_eq!(c, 0, "worktree identity: {e}");
        let (c, o, e) = forge(
            &path,
            &["forge", "issue", "new", &format!("issue from {wt}")],
        );
        assert_eq!(c, 0, "issue new in worktree: {e}");
        // Parse the allocated id from the output (ids are shared/sequential).
        let id: u64 = o["issue #".len()..]
            .split(' ')
            .next()
            .unwrap()
            .parse()
            .expect("parse allocated id");
        let evs = chain_events(&dir, &format!("refs/forge/issues/{id}"));
        assert!(
            evs[0].contains(&format!("\"actor\":\"{email}\"")),
            "worktree {wt} actor must be {email}: {}",
            evs[0]
        );
        actors.push(email.to_string());
    }
    // Three events, three DISTINCT actors, one repo.
    let unique: std::collections::HashSet<&String> = actors.iter().collect();
    assert_eq!(
        unique.len(),
        3,
        "actors must be pairwise distinct: {actors:?}"
    );
}
