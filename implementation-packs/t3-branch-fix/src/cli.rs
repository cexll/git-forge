//! CLI layer (t1b): `git forge issue` subcommands and the `git-issue` wrapper.
//!
//! All storage comes from t1a's `store`; this module adds command parsing and
//! human-readable output only. Entry is `git forge issue <sub> ...` (invoked by
//! main) and the thin `git-issue` wrapper forwards to the issue command.

use std::collections::HashMap;

use crate::event::{Event, EventKind, JsonValue};
use crate::store::{EventStore, StoreError};

/// Parse `"<n>"` into a positive u64, else a clean usage error (no panic).
fn parse_entity_id(s: &str) -> Result<u64, String> {
    if s.is_empty() {
        return Err("empty entity id".to_string());
    }
    let n: u64 = s
        .parse()
        .map_err(|_| format!("invalid entity id '{s}' (expected a positive integer)"))?;
    if n == 0 {
        return Err("entity id must be positive".to_string());
    }
    Ok(n)
}

fn json_str(value: &str) -> JsonValue {
    JsonValue::String(value.to_string())
}

fn body_obj(pairs: &[(&str, JsonValue)]) -> HashMap<String, JsonValue> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.clone()))
        .collect()
}

/// Build an `issue.*` event for `store`.
fn issue_event(kind: EventKind, id: u64, body: HashMap<String, JsonValue>) -> Event {
    Event::new(
        kind,
        "issue",
        id,
        // Actor comes from the running user's git identity in production; for
        // the store layer the actor is a caller-supplied string. The CLI uses
        // the configured identity when available, else a default.
        "git-forge",
        body,
    )
}

/// `git forge issue new <title> [description]`
fn cmd_new(store: &EventStore, args: &[String]) -> Result<String, String> {
    if args.is_empty() || args[0].trim().is_empty() {
        return Err(
            "usage: git forge issue new <title> [description] — title is required and non-empty"
                .into(),
        );
    }
    let title = args[0].trim().to_string();
    let description = args
        .get(1)
        .map(|d| d.trim().to_string())
        .filter(|d| !d.is_empty());
    let id = store.allocate_id().map_err(|e| e.to_string())?;
    let mut body = body_obj(&[("title", json_str(&title))]);
    if let Some(d) = description {
        body.insert("description".into(), json_str(&d));
    }
    let ev = issue_event(EventKind::IssueCreated, id, body);
    store
        .append_event(&crate::store::issue_ref(id), &ev)
        .map_err(|e| e.to_string())?;
    Ok(format!("issue #{id} created: {title}"))
}

/// `git forge issue list`
fn cmd_list(store: &EventStore) -> Result<String, String> {
    // Enumerate issues by walking allocated ids is not possible without a
    // registry; instead we read known refs by scanning counter state is not
    // part of t1a. L1 lists issues by presenting the event chains discoverable
    // via a best-effort scan of refs/forge/issues/*. We implement a documented
    // approximation: read the counter's next to bound the scan.
    let mut out = String::new();
    let mut found = 0usize;
    // Bound the scan by the counter's next value (best-effort).
    let next_opt = counter_next(store).ok();
    let bound = next_opt.unwrap_or(0);
    for n in 1..bound {
        let r = crate::store::issue_ref(n);
        if !store_has_ref(store, &r) {
            continue;
        }
        let chain = store.read_chain(&r).map_err(|e| e.to_string())?;
        if chain.is_empty() {
            continue;
        }
        let state = crate::event::fold(&chain).issue;
        out.push_str(&format!(
            "#{} {} ({})\n",
            state.id,
            state.title.as_deref().unwrap_or("(untitled)"),
            if state.open { "open" } else { "closed" }
        ));
        found += 1;
    }
    if found == 0 {
        return Ok("(no issues)".to_string());
    }
    Ok(out.trim_end().to_string())
}

/// `git forge issue show <n>`
fn cmd_show(store: &EventStore, args: &[String]) -> Result<String, String> {
    let id = parse_entity_id(args.first().map(|s| s.as_str()).unwrap_or(""))?;
    let r = crate::store::issue_ref(id);
    if !store_has_ref(store, &r) {
        return Err(format!("issue #{id} does not exist"));
    }
    let chain = store.read_chain(&r).map_err(|e| e.to_string())?;
    if chain.is_empty() {
        return Err(format!("issue #{id} has no events"));
    }
    let state = crate::event::fold(&chain).issue;
    let mut out = format!(
        "#{} {} — {}\n",
        state.id,
        state.title.as_deref().unwrap_or("(untitled)"),
        if state.open { "open" } else { "closed" }
    );
    if let Some(d) = &state.description {
        out.push_str(&format!("description: {d}\n"));
    }
    out.push('\n');
    if !state.comments.is_empty() {
        out.push_str("comments:\n");
        for c in &state.comments {
            out.push_str(&format!("  - {c}\n"));
        }
    }
    Ok(out.trim_end().to_string())
}

/// `git forge issue comment <n> <body>`
fn cmd_comment(store: &EventStore, args: &[String]) -> Result<String, String> {
    if args.len() < 2 {
        return Err("usage: git forge issue comment <n> <body>".into());
    }
    let id = parse_entity_id(&args[0])?;
    let body = args[1..].join(" ").trim().to_string();
    if body.is_empty() {
        return Err("comment body must be non-empty".into());
    }
    let r = crate::store::issue_ref(id);
    if !store_has_ref(store, &r) {
        return Err(format!("issue #{id} does not exist"));
    }
    let ev = issue_event(
        EventKind::IssueComment,
        id,
        body_obj(&[("body", json_str(&body))]),
    );
    store.append_event(&r, &ev).map_err(|e| e.to_string())?;
    Ok(format!("comment added to issue #{id}"))
}

/// `git forge issue close <n>` / `reopen <n>`
fn cmd_state(store: &EventStore, kind: EventKind, args: &[String]) -> Result<String, String> {
    let id = parse_entity_id(args.first().map(|s| s.as_str()).unwrap_or(""))?;
    let r = crate::store::issue_ref(id);
    if !store_has_ref(store, &r) {
        return Err(format!("issue #{id} does not exist"));
    }
    let ev = issue_event(kind, id, HashMap::new());
    store.append_event(&r, &ev).map_err(|e| e.to_string())?;
    let verb = match kind {
        EventKind::IssueClose => "closed",
        EventKind::IssueReopen => "reopened",
        _ => unreachable!(),
    };
    Ok(format!("issue #{id} {verb}"))
}

/// Read the counter's next value (best-effort) to bound `list`.
fn counter_next(store: &EventStore) -> Result<u64, StoreError> {
    let repo = store.repo();
    match repo.find_reference(crate::store::COUNTER_REF) {
        Ok(r) => {
            let tip = r.target().ok_or(StoreError::MissingRef)?;
            let commit = repo.find_commit(tip)?;
            let tree = repo.find_tree(commit.tree_id())?;
            let entry = tree
                .get_path(std::path::Path::new(".forge/counter.json"))
                .map_err(|_| StoreError::InvalidState("counter json missing".into()))?;
            let obj = entry.to_object(repo)?;
            let blob = obj
                .as_blob()
                .ok_or_else(|| StoreError::InvalidState("counter not a blob".into()))?;
            let content = std::str::from_utf8(blob.content())
                .map_err(|_| StoreError::InvalidState("counter not utf8".into()))?;
            let key = "\"next\":";
            let idx = content
                .find(key)
                .ok_or_else(|| StoreError::InvalidState("next missing".into()))?
                + key.len();
            let digits: String = content[idx..]
                .chars()
                .take_while(|c| c.is_ascii_digit())
                .collect();
            digits
                .parse::<u64>()
                .map_err(|_| StoreError::InvalidState("next not u64".into()))
        }
        Err(e) if e.code() == git2::ErrorCode::NotFound => Ok(1),
        Err(e) => Err(StoreError::Git(e)),
    }
}

fn store_has_ref(store: &EventStore, r: &str) -> bool {
    store.repo().find_reference(r).is_ok()
}

/// Dispatch a `git forge issue` subcommand. `argv` excludes the `issue` token.
pub fn run_issue(argv: &[String]) -> Result<String, String> {
    let store = EventStore::open(".").map_err(|e| format!("{e}"))?;
    let sub = argv.first().map(|s| s.as_str()).unwrap_or("");
    match sub {
        "new" => cmd_new(&store, &argv[1..]),
        "list" => cmd_list(&store),
        "show" => cmd_show(&store, &argv[1..]),
        "comment" => cmd_comment(&store, &argv[1..]),
        "close" => cmd_state(&store, EventKind::IssueClose, &argv[1..]),
        "reopen" => cmd_state(&store, EventKind::IssueReopen, &argv[1..]),
        "help" | "-h" | "--help" => Ok(issue_help()),
        "" => Ok(issue_help()),
        other => Err(format!("unknown issue subcommand '{other}'")),
    }
}

fn issue_help() -> String {
    "usage: git forge issue <new|list|show|comment|close|reopen> ...\n\
     \nsubcommands:\n\
     \x20 new <title> [description]\n\
     \x20 list\n\
     \x20 show <n>\n\
     \x20 comment <n> <body>\n\
     \x20 close <n>\n\
     \x20 reopen <n>"
        .to_string()
}

// ─────────────────────────── PR commands ───────────────────────────

/// Resolve a `--source`/`--base` argument to a canonical local branch OID, or
/// an Err with a clear message. Accepts only `refs/heads/<name>`: tags,
/// remote-tracking refs, OIDs, and revision expressions are rejected.
fn resolve_local_branch(store: &EventStore, arg: &str) -> Result<git2::Oid, String> {
    if arg.is_empty() {
        return Err("branch name must be non-empty".into());
    }
    if arg.starts_with("refs/heads/") {
        return store
            .repo()
            .find_reference(arg)
            .ok()
            .and_then(|r| r.target())
            .ok_or_else(|| format!("no such local branch '{arg}'"));
    }
    // Must be a bare local branch name: no refs/tags/, no refs/remotes/, not
    // a 40-hex OID, not a revision expression. Slashes are legal inside
    // branch names (feat/foo), so resolve refs/heads/<arg> and reject only
    // when no such local branch exists — the same rule as for plain names.
    let full = format!("refs/heads/{arg}");
    store
        .repo()
        .find_reference(&full)
        .ok()
        .and_then(|r| r.target())
        .ok_or_else(|| {
            format!(
                "'{arg}' is not a canonical local branch; use a plain branch name (tags, remote-tracking refs, OIDs, and revision expressions are rejected)"
            )
        })
}

/// `git merge-base --all <a> <b>` count must be exactly 1. Zero covers
/// unrelated/shallow histories (shallow → ask to deepen); multiple covers
/// criss-cross. Returns the single merge-base OID.
fn require_single_merge_base(
    repo: &git2::Repository,
    a: git2::Oid,
    b: git2::Oid,
) -> Result<git2::Oid, String> {
    use std::process::Command;
    let out = Command::new("git")
        .arg("-C")
        .arg(repo.path())
        .args(["merge-base", "--all", &a.to_string(), &b.to_string()])
        .output()
        .map_err(|e| format!("git merge-base failed: {e}"))?;
    let stdout =
        std::str::from_utf8(&out.stdout).map_err(|_| "merge-base output not utf8".to_string())?;
    let lines: Vec<&str> = stdout.trim().lines().filter(|l| !l.is_empty()).collect();
    // `git merge-base --all` exits 1 with EMPTY stdout when the two commits
    // share no common ancestor (unrelated histories, or shallow/incomplete
    // history). Exit 128 (nonempty stderr) is a genuine git failure. Without
    // this classification the deepen/unshallow guidance below is dead code.
    if !out.status.success() {
        if out.status.code() == Some(1) && lines.is_empty() {
            return Err(zero_merge_base_error(repo));
        }
        return Err(format!(
            "git merge-base failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    if lines.len() != 1 {
        if lines.is_empty() {
            return Err(zero_merge_base_error(repo));
        }
        return Err(format!(
            "multiple merge bases ({}) suggest a criss-cross history; cannot create the PR",
            lines.len()
        ));
    }
    lines[0]
        .trim()
        .parse::<git2::Oid>()
        .map_err(|_| "invalid merge-base oid".to_string())
}

/// User-facing error for zero merge bases: unrelated histories (no common
/// ancestor) vs shallow/incomplete history (must deepen/unshallow).
fn zero_merge_base_error(repo: &git2::Repository) -> String {
    if repo.is_shallow() {
        "no merge base (shallow history) — deepen/unshallow the repository first".to_string()
    } else {
        "no merge base — the source/base histories are unrelated, or history is shallow; deepen/unshallow before creating a PR".to_string()
    }
}

/// `git forge pr create --source <branch> --base <branch> <title>`
fn cmd_pr_create(store: &EventStore, args: &[String]) -> Result<String, String> {
    let mut source = None;
    let mut base = None;
    let mut title = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--source" => {
                i += 1;
                if i >= args.len() {
                    return Err("--source requires a branch name".into());
                }
                source = Some(args[i].clone());
            }
            "--base" => {
                i += 1;
                if i >= args.len() {
                    return Err("--base requires a branch name".into());
                }
                base = Some(args[i].clone());
            }
            a if a.starts_with("--") || a.starts_with('-') => {
                return Err(format!("unknown option '{a}'"));
            }
            t if title.is_none() => title = Some(t.to_string()),
            _ => return Err("too many positional arguments; usage: git forge pr create --source <branch> --base <branch> <title>".into()),
        }
        i += 1;
    }
    let source = source.ok_or_else(|| {
        String::from("usage: git forge pr create --source <branch> --base <branch> <title>")
    })?;
    let base = base.ok_or_else(|| {
        String::from("usage: git forge pr create --source <branch> --base <branch> <title>")
    })?;
    let title = title.ok_or_else(|| {
        String::from("usage: git forge pr create --source <branch> --base <branch> <title>")
    })?;
    if title.trim().is_empty() {
        return Err("PR title is required and must be non-empty".into());
    }

    let source_oid = resolve_local_branch(store, &source)?;
    let base_oid = resolve_local_branch(store, &base)?;
    if source == base {
        return Err("source and base branch must differ (no self-PR)".into());
    }
    if source_oid == base_oid {
        return Err("source and base branches resolve to the same commit (no self-PR)".into());
    }
    let merge_base = require_single_merge_base(store.repo(), base_oid, source_oid)?;

    let id = store
        .create_pr(
            title.trim(),
            &source,
            &base,
            source_oid,
            base_oid,
            merge_base,
        )
        .map_err(|e| e.to_string())?;
    Ok(format!("PR #{id} created: {title} ({source} -> {base})"))
}

/// `git forge pr show <n>`
fn cmd_pr_show(store: &EventStore, args: &[String]) -> Result<String, String> {
    let id = parse_entity_id(args.first().map(|s| s.as_str()).unwrap_or(""))?;
    let r = crate::store::pr_head_ref(id);
    if !store_has_ref(store, &r) {
        return Err(format!("PR #{id} does not exist"));
    }
    let chain = store.read_chain(&r).map_err(|e| e.to_string())?;
    if chain.is_empty() {
        return Err(format!("PR #{id} has no events"));
    }
    let state = crate::event::fold(&chain).pr;
    let mut out = format!(
        "PR #{} {} — {}\n",
        state.id,
        state.title.as_deref().unwrap_or("(untitled)"),
        state
            .effective_decision
            .as_deref()
            .map(|d| format!("decision: {d}"))
            .unwrap_or_else(|| "no review yet".into())
    );
    if let Some(r) = &state.merge_result {
        out.push_str(&format!("merged: {r}\n"));
    }
    if let Some(r) = &state.base_ref {
        out.push_str(&format!("base: {r}\n"));
    }
    if let Some(r) = &state.source_ref {
        out.push_str(&format!("source: {r}\n"));
    }
    if let (Some(b), Some(s)) = (&state.base_head, &state.source_head) {
        out.push_str(&format!("diff: {b}...{s}\n"));
    }
    for c in &state.comments {
        out.push_str(&format!("comment: {c}\n"));
    }
    Ok(out.trim_end().to_string())
}

/// `git forge pr list`
fn cmd_pr_list(store: &EventStore) -> Result<String, String> {
    let bound = counter_next(store).ok().unwrap_or(1);
    let mut out = String::new();
    let mut found = 0usize;
    for n in 1..bound {
        let r = crate::store::pr_head_ref(n);
        if !store_has_ref(store, &r) {
            continue;
        }
        let chain = store.read_chain(&r).map_err(|e| e.to_string())?;
        if chain.is_empty() {
            continue;
        }
        let st = crate::event::fold(&chain).pr;
        out.push_str(&format!(
            "PR #{} {} ({})\n",
            st.id,
            st.title.as_deref().unwrap_or("(untitled)"),
            st.effective_decision.as_deref().unwrap_or("no review")
        ));
        found += 1;
    }
    if found == 0 {
        return Ok("(no pull requests)".into());
    }
    Ok(out.trim_end().to_string())
}

/// `git forge pr comment <n> <body>`
fn cmd_pr_comment(store: &EventStore, args: &[String]) -> Result<String, String> {
    if args.len() < 2 {
        return Err("usage: git forge pr comment <n> <body>".into());
    }
    let id = parse_entity_id(&args[0])?;
    let body = args[1..].join(" ").trim().to_string();
    if body.is_empty() {
        return Err("comment body must be non-empty".into());
    }
    let r = crate::store::pr_head_ref(id);
    if !store_has_ref(store, &r) {
        return Err(format!("PR #{id} does not exist"));
    }
    let ev = Event::new(
        EventKind::PrComment,
        "pr",
        id,
        "git-forge",
        body_obj(&[("body", json_str(&body))]),
    );
    store.append_event(&r, &ev).map_err(|e| e.to_string())?;
    Ok(format!("comment added to PR #{id}"))
}

/// `git forge pr review <n> --approve|--reject [--file <f> --line <l> --commit <c>]`
fn cmd_pr_review(store: &EventStore, args: &[String]) -> Result<String, String> {
    let id = parse_entity_id(args.first().map(|s| s.as_str()).unwrap_or(""))?;
    let mut decision = None;
    let mut file = None;
    let mut line = None;
    let mut commit = None;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--approve" => decision = Some("approve".to_string()),
            "--reject" => decision = Some("reject".to_string()),
            "--file" => {
                i += 1;
                if i >= args.len() {
                    return Err("--file requires a path".into());
                }
                file = Some(args[i].clone());
            }
            "--line" => {
                i += 1;
                if i >= args.len() {
                    return Err("--line requires a number".into());
                }
                line = Some(args[i].clone());
            }
            "--commit" => {
                i += 1;
                if i >= args.len() {
                    return Err("--commit requires a hash".into());
                }
                commit = Some(args[i].clone());
            }
            a if a.starts_with('-') => return Err(format!("unknown option '{a}'")),
            _ => {}
        }
        i += 1;
    }
    let decision = decision
        .ok_or_else(|| String::from("usage: git forge pr review <n> --approve|--reject"))?;
    let r = crate::store::pr_head_ref(id);
    if !store_has_ref(store, &r) {
        return Err(format!("PR #{id} does not exist"));
    }
    // FR-005: inline review comments anchor to a commit hash. --file/--line
    // are only meaningful as inline comments, so they require the anchor;
    // an unanchored inline review would not reference anything immutable.
    if (file.is_some() || line.is_some()) && commit.is_none() {
        return Err(
            "inline review requires --commit <hash> to anchor the comment (FR-005); \
             add --commit or drop --file/--line"
                .into(),
        );
    }
    // The anchor must resolve to a real commit object. It is intentionally NOT
    // constrained to the PR snapshot: spec.md explicitly permits inline
    // comments on commits outside the PR snapshot (anchored reference). The
    // resolved commit OID is what gets stored — never the raw input — so a
    // mutable ref like `main` cannot smuggle a moving anchor into the event.
    let commit = match commit {
        Some(c) => Some(
            store
                .repo()
                .revparse_single(&c)
                .and_then(|o| o.peel_to_commit())
                .map(|c| c.id().to_string())
                .map_err(|_| {
                    format!(
                        "inline review --commit '{c}' does not resolve to a commit \
                         (FR-005 anchor must be a real commit)"
                    )
                })?,
        ),
        None => None,
    };
    let mut body = HashMap::new();
    body.insert("decision".into(), json_str(&decision));
    if let Some(f) = file {
        body.insert("file".into(), json_str(&f));
    }
    if let Some(l) = line {
        body.insert("line".into(), json_str(&l));
    }
    if let Some(c) = commit {
        // Inline comment anchored to a commit (immutable; never follows later
        // diff changes).
        body.insert("commit".into(), json_str(&c));
    }
    let ev = Event::new(EventKind::PrReview, "pr", id, "git-forge", body);
    store.append_event(&r, &ev).map_err(|e| e.to_string())?;
    Ok(format!("reviewed PR #{id} ({decision})"))
}

/// `git forge pr diff <n>` — three-dot diff from the immutable snapshot refs.
fn cmd_pr_diff(store: &EventStore, args: &[String]) -> Result<String, String> {
    use std::process::Command;
    let id = parse_entity_id(args.first().map(|s| s.as_str()).unwrap_or(""))?;
    let head = crate::store::pr_head_ref(id);
    if !store_has_ref(store, &head) {
        return Err(format!("PR #{id} does not exist"));
    }
    // Resolve from the IMMUTABLE snapshot refs (survive branch deletion + gc).
    let source = crate::store::pr_source_ref(id);
    let base = crate::store::pr_base_ref(id);
    let source_oid = store
        .repo()
        .find_reference(&source)
        .and_then(|r| r.target().ok_or(git2::Error::from_str("no target")))
        .map_err(|_| format!("PR #{id} snapshot source ref missing"))?;
    let base_oid = store
        .repo()
        .find_reference(&base)
        .and_then(|r| r.target().ok_or(git2::Error::from_str("no target")))
        .map_err(|_| format!("PR #{id} snapshot base ref missing"))?;
    let out = Command::new("git")
        .arg("-C")
        .arg(store.repo().path())
        .args(["diff", &format!("{base_oid}...{source_oid}")])
        .output()
        .map_err(|e| format!("git diff failed: {e}"))?;
    if !out.status.success() {
        return Err("git diff failed".into());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim_end().to_string())
}

/// Run `git` in the given working directory, returning (success, stdout,
/// stderr). Used for worktree merge/rebase execution and cleanup.
fn git_in(dir: &std::path::Path, args: &[&str]) -> (bool, String, String) {
    use std::process::Command;
    let out = Command::new("git").arg("-C").arg(dir).args(args).output();
    match out {
        Ok(o) => (
            o.status.success(),
            String::from_utf8_lossy(&o.stdout).trim().to_string(),
            String::from_utf8_lossy(&o.stderr).trim().to_string(),
        ),
        Err(e) => (false, String::new(), format!("failed to spawn git: {e}")),
    }
}

/// True if `git rebase` is mid-flight in the worktree (either rebase-merge or
/// rebase-apply state dir exists, resolved via `git rev-parse --git-path`).
fn rebase_in_progress(dir: &std::path::Path) -> bool {
    for sub in ["rebase-merge", "rebase-apply"] {
        let (ok, out, _) = git_in(dir, &["rev-parse", "--git-path", sub]);
        if ok {
            let p = out.trim();
            if !p.is_empty() {
                let abs = if p.starts_with('/') || dir.is_absolute() {
                    let base = std::path::Path::new(p);
                    if base.is_absolute() {
                        base.to_path_buf()
                    } else {
                        dir.join(base)
                    }
                } else {
                    dir.join(p)
                };
                if abs.exists() {
                    return true;
                }
            }
        }
    }
    false
}

/// True if `base_ref` is checked out in a non-temporary worktree (main or a
/// linked one). Refuses advancing a checked-out branch without updating its
/// working tree (wire contract § Checked-out base guard). Aborts (Err) if the
/// worktree list cannot be enumerated — a failed guard must not silently
/// bypass the check.
fn base_checked_out_elsewhere(store: &EventStore, base_ref: &str) -> Result<bool, String> {
    use std::process::Command;
    let repo_dir = store
        .repo()
        .workdir()
        .map(|p| p.to_path_buf())
        .ok_or_else(|| "cannot resolve repository working directory".to_string())?;
    let out = Command::new("git")
        .arg("-C")
        .arg(&repo_dir)
        .args(["worktree", "list", "--porcelain"])
        .output()
        .map_err(|e| format!("git worktree list failed: {e}"))?;
    if !out.status.success() {
        return Err("git worktree list failed".into());
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let want = format!("branch refs/heads/{base_ref}");
    Ok(text.lines().any(|l| l.trim() == want))
}

/// `git forge pr merge [<n>] [--squash|--rebase]`
///
/// Fold the PR chain, require effective review decision = approve, then merge
/// the immutable snapshot (`source_head` → `base_ref` tip) via the git binary
/// in a temporary worktree, and atomically finalize: delete pending result
/// ref + CAS base branch + CAS PR head chain in ONE ref transaction.
fn cmd_pr_merge(store: &EventStore, args: &[String]) -> Result<String, String> {
    let mut id_arg = None;
    let mut strategy: Option<&str> = None; // merge | squash | rebase
    for a in args {
        match a.as_str() {
            "--merge" => {
                if strategy.is_some() {
                    return Err("merge strategy flag specified more than once".into());
                }
                strategy = Some("merge");
            }
            "--squash" => {
                if strategy.is_some() {
                    return Err("merge strategy flag specified more than once".into());
                }
                strategy = Some("squash");
            }
            "--rebase" => {
                if strategy.is_some() {
                    return Err("merge strategy flag specified more than once".into());
                }
                strategy = Some("rebase");
            }
            "-h" | "--help" => return Ok(pr_merge_help()),
            s if s.starts_with('-') => return Err(format!("unknown option '{s}'")),
            s => {
                if id_arg.is_some() {
                    return Err(
                        "usage: git forge pr merge [<n>] [--merge|--squash|--rebase] \
                                (only one PR id allowed)"
                            .into(),
                    );
                }
                id_arg = Some(s.to_string());
            }
        }
    }
    let strategy = strategy.unwrap_or("merge");
    let id = parse_entity_id(id_arg.as_deref().unwrap_or(""))?;
    let head = crate::store::pr_head_ref(id);
    if !store_has_ref(store, &head) {
        return Err(format!("PR #{id} does not exist"));
    }
    let chain = store.read_chain(&head).map_err(|e| e.to_string())?;
    let state = crate::event::fold(&chain).pr;

    // Gate: effective approve required (last reachable pr.review).
    if state.effective_decision.as_deref() != Some("approve") {
        return Err(format!(
            "PR #{id} is not approved (effective decision: {})",
            state.effective_decision.as_deref().unwrap_or("none")
        ));
    }
    // Already merged: a pr.merge event exists → no double merge.
    if state.merge_result.is_some() {
        return Err(format!("PR #{id} is already merged"));
    }
    let base_ref = state
        .base_ref
        .clone()
        .ok_or_else(|| "PR has no base_ref".to_string())?;
    let source_oid = store
        .repo()
        .find_reference(&crate::store::pr_source_ref(id))
        .and_then(|r| r.target().ok_or(git2::Error::from_str("no target")))
        .map_err(|_| format!("PR #{id} snapshot source ref missing"))?;
    let base_oid = store
        .repo()
        .find_reference(&crate::store::pr_base_ref(id))
        .and_then(|r| r.target().ok_or(git2::Error::from_str("no target")))
        .map_err(|_| format!("PR #{id} snapshot base ref missing"))?;
    let merge_base = state
        .merge_base
        .as_deref()
        .map(|s| s.parse::<git2::Oid>())
        .transpose()
        .map_err(|_| "PR merge_base invalid".to_string())?
        .ok_or_else(|| "PR has no merge_base".to_string())?;

    // Stale-base rejection: current base branch tip must equal PR base_head.
    let current_base = store
        .repo()
        .find_reference(&format!("refs/heads/{base_ref}"))
        .and_then(|r| {
            r.target()
                .ok_or(git2::Error::from_str("base branch has no target"))
        })
        .map_err(|_| format!("base branch '{base_ref}' does not exist"))?;
    if current_base != base_oid {
        return Err(format!(
            "base branch '{base_ref}' has moved since PR #{id} was created \
             ({current_base} != snapshot {base_oid}); recreate the PR or merge manually"
        ));
    }

    // Checked-out base guard: refuse if base is checked out anywhere.
    if base_checked_out_elsewhere(store, &base_ref)? {
        return Err(format!(
            "base branch '{base_ref}' is checked out in a worktree; \
             run the merge from/against an un-checked-out base"
        ));
    }

    // Temporary worktree, detached at the immutable base OID. worktree
    // add/remove/list are REPOSITORY-level commands: run them from the main
    // worktree, never from the temp dir's parent.
    let repo_dir = store
        .repo()
        .workdir()
        .map(|p| p.to_path_buf())
        .ok_or_else(|| "cannot resolve repository working directory".to_string())?;
    // Repo-specific temp worktree path: hash the canonical workdir so parallel
    // test repos (same pid) never collide on the global temp dir.
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    repo_dir.to_string_lossy().as_bytes().hash(&mut hasher);
    let nonce = hasher.finish();
    // Rebase must start detached at /source so `git rebase --onto /base
    // <merge_base>` can replay the snapshot commits; everything else starts
    // detached at the immutable /base OID.
    let detach_oid = if strategy == "rebase" {
        source_oid
    } else {
        base_oid
    };
    // Reserve the temp path atomically via a SIBLING lock file (`create_new`,
    // O_EXCL): two concurrent merges on the same repo compute the same path,
    // so exactly one wins the lock and the loser retries with a fresh suffix.
    // `git worktree add` is then free to create the directory itself. We never
    // touch a path we do not own.
    let mut attempt = 0u32;
    let mk = |attempt: u32| {
        std::env::temp_dir().join(format!(
            "git-forge-pr{id}-merge-{nonce:x}-{}-{attempt}",
            std::process::id()
        ))
    };
    let mut tmp = mk(0);
    // The lock handle is held until the worktree is removed AND verified gone,
    // so no concurrent same-repo merge can reuse this path while we own it.
    let mut lock_handle: Option<std::fs::File> = None;
    let (ok_wt, err_wt) = loop {
        let lock = tmp.with_extension("lock");
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock)
        {
            Ok(h) => {
                lock_handle = Some(h);
                let (ok, _, err) = git_in(
                    &repo_dir,
                    &[
                        "worktree",
                        "add",
                        "--detach",
                        tmp.to_str().unwrap_or("/tmp/none"),
                        &detach_oid.to_string(),
                    ],
                );
                break (ok, err);
            }
            Err(_) if attempt < 16 => {
                attempt += 1;
                tmp = mk(attempt);
            }
            Err(_) => {
                break (
                    false,
                    format!(
                        "could not reserve a unique temp worktree path after {attempt} attempts"
                    ),
                );
            }
        }
    };
    if !ok_wt {
        // Release our own lock; the path was never registered to us.
        drop(lock_handle.take());
        let _ = std::fs::remove_file(tmp.with_extension("lock"));
        return Err(format!("failed to create temporary worktree: {err_wt}"));
    }

    // Release the path lock (drop handle + remove file) so a concurrent
    // same-repo merge can reuse the path. Must run on EVERY early return after
    // the worktree was added (strategy failures go through cleanup_failed_worktree
    // which also drops the file; removal/list/barrier/finalize returns call this).
    let release_lock = |lock_handle: &mut Option<std::fs::File>, path: &std::path::Path| {
        *lock_handle = None;
        let _ = std::fs::remove_file(path.with_extension("lock"));
    };

    // Execute the strategy inside the worktree. result_commit = worktree HEAD.
    let result_commit = match strategy {
        "rebase" => {
            let (ok, _o, e) = git_in(
                &tmp,
                &[
                    "rebase",
                    "--onto",
                    &base_oid.to_string(),
                    &merge_base.to_string(),
                ],
            );
            if !ok {
                return Err(cleanup_failed_worktree(&repo_dir, &tmp, "rebase", e));
            }
            let (ok2, out2, e2) = git_in(&tmp, &["rev-parse", "HEAD"]);
            if !ok2 {
                return Err(cleanup_failed_worktree(&repo_dir, &tmp, "rebase", e2));
            }
            out2
        }
        "squash" => {
            let (ok, _o, e) = git_in(&tmp, &["merge", "--squash", &source_oid.to_string()]);
            if !ok {
                return Err(cleanup_failed_worktree(&repo_dir, &tmp, "squash", e));
            }
            let title = state.title.as_deref().ok_or_else(|| {
                cleanup_failed_worktree(&repo_dir, &tmp, "squash", "PR has no title".to_string())
            })?;
            let (ok2, _o2, e2) = git_in(&tmp, &["commit", "-m", title]);
            if !ok2 {
                return Err(cleanup_failed_worktree(&repo_dir, &tmp, "squash", e2));
            }
            let (ok3, out3, e3) = git_in(&tmp, &["rev-parse", "HEAD"]);
            if !ok3 {
                return Err(cleanup_failed_worktree(&repo_dir, &tmp, "squash", e3));
            }
            out3
        }
        _ => {
            // default merge commit; --no-ff --no-edit, never opens an editor.
            let (ok, _o, e) = git_in(
                &tmp,
                &["merge", "--no-ff", "--no-edit", &source_oid.to_string()],
            );
            if !ok {
                return Err(cleanup_failed_worktree(&repo_dir, &tmp, "merge", e));
            }
            let (ok2, out2, e2) = git_in(&tmp, &["rev-parse", "HEAD"]);
            if !ok2 {
                return Err(cleanup_failed_worktree(&repo_dir, &tmp, "merge", e2));
            }
            out2
        }
    };
    let result_commit = result_commit.parse::<git2::Oid>().map_err(|_| {
        cleanup_failed_worktree(&repo_dir, &tmp, "merge", "invalid result commit".into())
    })?;

    // Keep result_commit reachable across worktree removal (GC could prune the
    // merge commit before the final transaction pins it).
    store
        .create_pending_result_ref(id, result_commit)
        .map_err(|e| cleanup_failed_worktree(&repo_dir, &tmp, "merge", e.to_string()))?;

    // Remove the temporary worktree, then verify it is gone. worktree
    // remove/list run from the repository.
    // Debug-only test seam (VAL-027): lock the temp worktree so
    // `git worktree remove --force` fails deterministically, exercising the
    // removal-failure branch (leftover report + best-effort pending-ref
    // cleanup). Inert in release builds and when the env var is unset.
    #[cfg(debug_assertions)]
    if std::env::var("GIT_FORGE_TEST_FAIL_WORKTREE_REMOVE").as_deref() == Ok("1") {
        let (lock_ok, _, lock_err) = git_in(
            &repo_dir,
            &["worktree", "lock", tmp.to_str().unwrap_or("/tmp/none")],
        );
        if !lock_ok {
            return Err(format!(
                "test seam: failed to lock temp worktree for removal-failure injection: {lock_err}"
            ));
        }
    }
    let (ok_rm, _o, err_rm) = git_in(
        &repo_dir,
        &[
            "worktree",
            "remove",
            "--force",
            tmp.to_str().unwrap_or("/tmp/none"),
        ],
    );
    if !ok_rm {
        // Best-effort: remove the pending result ref (CAS expected OID); the
        // worktree itself stays (can't force-clean a failed removal safely).
        // Report only if the pending ref remains.
        release_lock(&mut lock_handle, &tmp);
        let left = match store.delete_pending_result_ref(id, result_commit) {
            Ok(true) | Ok(false) => false,
            Err(_) => true,
        };
        let pending = if left {
            format!("; pending result ref refs/forge/prs/{id}/result left in place")
        } else {
            String::new()
        };
        return Err(format!(
            "merge succeeded but temp worktree removal failed: {err_rm} \
             (worktree left at {}{pending})",
            tmp.display()
        ));
    }
    // `git worktree remove` already removed the directory; do NOT run a
    // recursive delete here (each path is owned by whichever merge holds its
    // sibling lock — deleting another process's worktree is never safe).
    // AC-005h: verify the disposable directory is actually gone.
    if tmp.exists() {
        release_lock(&mut lock_handle, &tmp);
        let left = match store.delete_pending_result_ref(id, result_commit) {
            Ok(true) | Ok(false) => false,
            Err(_) => true,
        };
        let pending = if left {
            format!("; pending result ref refs/forge/prs/{id}/result left in place")
        } else {
            String::new()
        };
        return Err(format!(
            "merge succeeded but temp worktree directory still exists at {}{pending}",
            tmp.display()
        ));
    }
    let (ok_l, list_l, err_l) = git_in(&repo_dir, &["worktree", "list", "--porcelain"]);
    if !ok_l {
        // Best-effort: remove the pending result ref; cannot verify the
        // worktree is gone → hard abort before any ref update.
        release_lock(&mut lock_handle, &tmp);
        let left = match store.delete_pending_result_ref(id, result_commit) {
            Ok(true) | Ok(false) => false,
            Err(_) => true,
        };
        let pending = if left {
            format!("; pending result ref refs/forge/prs/{id}/result left in place")
        } else {
            String::new()
        };
        return Err(format!(
            "merge succeeded but worktree verification failed (git worktree list: {err_l}){pending}"
        ));
    }
    if list_l.contains(&tmp.to_string_lossy().to_string()) {
        // Best-effort: remove the pending result ref; the stale registration
        // is reported but the result commit is not left dangling.
        let left = match store.delete_pending_result_ref(id, result_commit) {
            Ok(true) | Ok(false) => false,
            Err(_) => true,
        };
        let pending = if left {
            format!("; pending result ref refs/forge/prs/{id}/result left in place")
        } else {
            String::new()
        };
        return Err(format!(
            "merge succeeded but temp worktree still registered at {}{pending}",
            tmp.display()
        ));
    }
    // Worktree removed AND verified gone: release our path lock so a concurrent
    // same-repo merge can reuse the path, then run barrier + final transaction.
    release_lock(&mut lock_handle, &tmp);

    // Test-only pending-window barrier: not part of the user CLI contract.
    #[cfg(debug_assertions)]
    if let Err(b) = maybe_run_test_barrier() {
        // Deadline failed: remove the pending ref best-effort (CAS expected
        // OID); report only if it remains.
        let left = match store.delete_pending_result_ref(id, result_commit) {
            Ok(true) | Ok(false) => false,
            Err(_) => true,
        };
        if left {
            return Err(format!(
                "{b} (pending result ref refs/forge/prs/{id}/result left in place)"
            ));
        }
        return Err(b);
    }

    // Single atomic completion transaction: delete pending ref + CAS base + CAS head.
    // finalize_pr_merge reads the head tip itself so all three move atomically.
    store
        .finalize_pr_merge(id, &base_ref, base_oid, result_commit, "git-forge")
        .map_err(|e| {
            // Nothing moved; report the leftover pending ref.
            format!(
                "merge execution finished but final transaction failed \
                 ({e}); refs unchanged, pending result ref refs/forge/prs/{id}/result left in place"
            )
        })?;
    Ok(format!("merged PR #{id} into {base_ref} ({result_commit})"))
}

/// Clean up a failed strategy execution: abort merge/rebase (rebase only when
/// state exists), run `git clean -fd`, remove the temp worktree. Returns the
/// user-facing error message.
fn cleanup_failed_worktree(
    repo_dir: &std::path::Path,
    tmp: &std::path::Path,
    kind: &str,
    cause: String,
) -> String {
    match kind {
        // Squash failure: `git merge --squash` stages without a MERGE_HEAD,
        // so `merge --abort` cannot reset. Reset the detached worktree back to
        // HEAD (= the /base OID it was added at), then clean + remove.
        "squash" => {
            let _ = git_in(tmp, &["reset", "--hard", "HEAD"]);
        }
        // Default merge failure: abort the in-flight merge (MERGE_HEAD state).
        "merge" => {
            let (state_ok, _, _) = git_in(tmp, &["rev-parse", "--verify", "-q", "MERGE_HEAD"]);
            if state_ok {
                let _ = git_in(tmp, &["merge", "--abort"]);
            }
        }
        // Rebase failure: abort only when rebase state exists.
        "rebase" => {
            if rebase_in_progress(tmp) {
                let _ = git_in(tmp, &["rebase", "--abort"]);
            }
        }
        _ => {}
    }
    let _ = git_in(tmp, &["clean", "-fd"]);
    let (ok_rm, _o, err_rm) = git_in(
        repo_dir,
        &[
            "worktree",
            "remove",
            "--force",
            tmp.to_str().unwrap_or("/tmp/none"),
        ],
    );
    if !ok_rm {
        // The registration is still live (e.g. the worktree is locked), so
        // deleting the directory underneath it would leave git's registration
        // pointing at a path that no longer exists — a dangling worktree
        // (AC-005d: no registration without its directory). Preserve the
        // leftover and report it so the operator can unlock/remove it.
        // Release the sibling lock so the suffix stays reusable.
        let _ = std::fs::remove_file(tmp.with_extension("lock"));
        return format!(
            "git {kind} failed: {cause}; temp worktree removal failed and the \
             worktree is left at \"{}\" ({err_rm}) — unlock and remove it manually",
            tmp.display()
        );
    }
    // `git worktree remove` already removed the directory; do NOT run a
    // recursive delete here (each path is owned by whichever merge holds its
    // sibling lock — deleting another process's worktree is never safe).
    // AC-005h: verify the disposable directory is actually gone.
    let _ = std::fs::remove_file(tmp.with_extension("lock"));
    format!("git {kind} failed: {cause}; worktree cleaned up, no ref changes made")
}

/// Test-only pending-window barrier (wire contract § Test-only pending-window
/// barrier). Debug builds only; inert in release. When
/// `GIT_FORGE_TEST_MERGE_BARRIER=<dir>` is set, the merge pauses after the
/// pending `/result` ref exists (and the temp worktree is gone) but before the
/// final transaction:
///   1. atomically create `<dir>/ready` (O_CREAT|O_EXCL);
///   2. poll for `<dir>/release` (bounded 30s deadline);
///   3. on success, delete `<dir>/release` and continue;
///   4. on deadline, remove both sentinels best-effort and fail the merge with
///      no ref updates.
#[cfg(debug_assertions)]
fn maybe_run_test_barrier() -> Result<(), String> {
    use std::io::Write;
    let Ok(dir) = std::env::var("GIT_FORGE_TEST_MERGE_BARRIER") else {
        return Ok(());
    };
    let dir = std::path::PathBuf::from(dir);
    std::fs::create_dir_all(&dir).map_err(|e| format!("barrier dir: {e}"))?;
    let ready = dir.join("ready");
    let release = dir.join("release");
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&ready)
        .map_err(|e| format!("barrier ready sentinel: {e}"))?;
    writeln!(f, "ready").ok();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while !release.exists() {
        if std::time::Instant::now() > deadline {
            let _ = std::fs::remove_file(&ready);
            let _ = std::fs::remove_file(&release);
            return Err("test merge barrier deadline exceeded; no ref updates made".into());
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    // Observed release: delete it and the ready sentinel, then continue.
    let _ = std::fs::remove_file(&release);
    let _ = std::fs::remove_file(&ready);
    Ok(())
}

fn pr_merge_help() -> String {
    "usage: git forge pr merge [<n>] [--merge|--squash|--rebase]\n\
     \x20 default / --merge: merge commit (--no-ff --no-edit)\n\
     \x20 --squash: single squashed commit\n\
     \x20 --rebase: replay source onto base (linear history)"
        .to_string()
}

/// Dispatch a `git forge pr` subcommand. `argv` excludes the `pr` token.
pub fn run_pr(argv: &[String]) -> Result<String, String> {
    let store = EventStore::open(".").map_err(|e| format!("{e}"))?;
    let sub = argv.first().map(|s| s.as_str()).unwrap_or("");
    match sub {
        "create" => cmd_pr_create(&store, &argv[1..]),
        "list" => cmd_pr_list(&store),
        "show" => cmd_pr_show(&store, &argv[1..]),
        "comment" => cmd_pr_comment(&store, &argv[1..]),
        "review" => cmd_pr_review(&store, &argv[1..]),
        "diff" => cmd_pr_diff(&store, &argv[1..]),
        "merge" => cmd_pr_merge(&store, &argv[1..]),
        "help" | "-h" | "--help" => Ok(pr_help()),
        "" => Ok(pr_help()),
        other => Err(format!("unknown pr subcommand '{other}'")),
    }
}

fn pr_help() -> String {
    "usage: git forge pr <create|list|show|comment|review|diff|merge> ...\n\
     \nsubcommands:\n\
     \x20 create --source <branch> --base <branch> <title>\n\
     \x20 list\n\
     \x20 show <n>\n\
     \x20 comment <n> <body>\n\
     \x20 review <n> --approve|--reject [--file <f> --line <l> --commit <c>]\n\
     \x20 diff <n>\n\
     \x20 merge [<n>] [--squash|--rebase]"
        .to_string()
}
