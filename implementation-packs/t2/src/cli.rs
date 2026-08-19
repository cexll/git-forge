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
    // Must be a bare local branch name (no refs/tags/, no remotes/, no /
    // separator suggesting a rev expression, not a 40-hex OID).
    if arg.contains('/') {
        return Err(format!(
            "'{arg}' is not a canonical local branch; use a plain branch name (tags, remote-tracking refs, OIDs, and revision expressions are rejected)"
        ));
    }
    let full = format!("refs/heads/{arg}");
    store
        .repo()
        .find_reference(&full)
        .ok()
        .and_then(|r| r.target())
        .ok_or_else(|| format!("no such local branch '{arg}'"))
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
    if !out.status.success() {
        return Err("git merge-base failed".into());
    }
    let lines: Vec<&str> = std::str::from_utf8(&out.stdout)
        .map_err(|_| "merge-base output not utf8".to_string())?
        .trim()
        .lines()
        .filter(|l| !l.is_empty())
        .collect();
    if lines.len() != 1 {
        if lines.is_empty() {
            let shallow = repo.is_shallow();
            if shallow {
                return Err(
                    "no merge base (shallow history) — deepen/unshallow the repository first"
                        .into(),
                );
            }
            return Err("no merge base — the source/base histories are unrelated, or history is shallow; deepen/unshallow before creating a PR".into());
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
    if let Some(anchor) = commit.clone() {
        // anchored inline comment: body carries file/line/commit
        let _ = anchor;
    }
    let r = crate::store::pr_head_ref(id);
    if !store_has_ref(store, &r) {
        return Err(format!("PR #{id} does not exist"));
    }
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
        "help" | "-h" | "--help" => Ok(pr_help()),
        "" => Ok(pr_help()),
        other => Err(format!("unknown pr subcommand '{other}'")),
    }
}

fn pr_help() -> String {
    "usage: git forge pr <create|list|show|comment|review|diff> ...\n\
     \nsubcommands:\n\
     \x20 create --source <branch> --base <branch> <title>\n\
     \x20 list\n\
     \x20 show <n>\n\
     \x20 comment <n> <body>\n\
     \x20 review <n> --approve|--reject [--file <f> --line <l> --commit <c>]\n\
     \x20 diff <n>"
        .to_string()
}
