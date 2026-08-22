//! CLI layer (t1b): `git forge issue` subcommands and the `git-issue` wrapper.
//!
//! All storage comes from t1a's `store`; this module adds command parsing and
//! human-readable output only. Entry is `git forge issue <sub> ...` (invoked by
//! main) and the thin `git-issue` wrapper forwards to the issue command.

use std::collections::HashMap;

use crate::event::{Event, EventKind, JsonValue};
use crate::identity::open_mutation_store;
use crate::store::EventStore;

/// Parse `"<n>"` into a positive u64, else a clean usage error (no panic).
pub(crate) fn parse_entity_id(s: &str) -> Result<u64, String> {
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
fn issue_event(kind: EventKind, id: u64, actor: &str, body: HashMap<String, JsonValue>) -> Event {
    Event::new(
        kind, "issue", id,
        // Wire contract: `"actor": "<user.email>"`. Every CLI command resolves
        // the invoking repo's identity ONCE via the store's `actor()` (git
        // config `user.email`, falling back to `forge@localhost` when no
        // identity is configured) and passes it through here — the actor is
        // never hardcoded.
        actor, body,
    )
}

/// `git forge issue new <title> [description]`
fn cmd_new(args: &[String]) -> Result<String, String> {
    // Pure argument validation first — a usage error must not be masked by a
    // later config-open failure (error precedence, F-028).
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
    // Identity + open + bind — malformed .git/config surfaces here as a clean
    // CLI error (git binary resolver), never as an libgit2 open/SIGSEGV.
    let (store, actor) = open_mutation_store()?;
    let id = store.allocate_id().map_err(|e| e.to_string())?;
    let mut body = body_obj(&[("title", json_str(&title))]);
    if let Some(d) = description {
        body.insert("description".into(), json_str(&d));
    }
    let ev = issue_event(EventKind::IssueCreated, id, &actor, body);
    store
        .append_event(&crate::store::issue_ref(id), &ev)
        .map_err(|e| e.to_string())?;
    Ok(format!("issue #{id} created: {title}"))
}

/// `git forge issue list`
fn cmd_list(store: &EventStore) -> Result<String, String> {
    // Issue list is a bounded scan of refs/forge/issues/1..counter_next
    // (best-effort when the counter is unreadable).
    let mut out = String::new();
    let mut found = 0usize;
    // Bound the scan by the counter's next value (best-effort).
    let next_opt = store.counter_next().ok();
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
fn cmd_comment(args: &[String]) -> Result<String, String> {
    if args.len() < 2 {
        return Err("usage: git forge issue comment <n> <body>".into());
    }
    let id = parse_entity_id(&args[0])?;
    let body = args[1..].join(" ").trim().to_string();
    if body.is_empty() {
        return Err("comment body must be non-empty".into());
    }
    let (store, actor) = open_mutation_store()?;
    let r = crate::store::issue_ref(id);
    if !store_has_ref(store.store(), &r) {
        return Err(format!("issue #{id} does not exist"));
    }
    let ev = issue_event(
        EventKind::IssueComment,
        id,
        &actor,
        body_obj(&[("body", json_str(&body))]),
    );
    store.append_event(&r, &ev).map_err(|e| e.to_string())?;
    Ok(format!("comment added to issue #{id}"))
}

/// `git forge issue close <n>` / `reopen <n>`
fn cmd_state(kind: EventKind, args: &[String]) -> Result<String, String> {
    let id = parse_entity_id(args.first().map(|s| s.as_str()).unwrap_or(""))?;
    let (store, actor) = open_mutation_store()?;
    let r = crate::store::issue_ref(id);
    if !store_has_ref(store.store(), &r) {
        return Err(format!("issue #{id} does not exist"));
    }
    let ev = issue_event(kind, id, &actor, HashMap::new());
    store.append_event(&r, &ev).map_err(|e| e.to_string())?;
    let verb = match kind {
        EventKind::IssueClose => "closed",
        EventKind::IssueReopen => "reopened",
        _ => unreachable!(),
    };
    Ok(format!("issue #{id} {verb}"))
}

/// True if the given ref exists in this repo (store-level existence check,
/// used by issue/PR commands before operating). Not a counter read — the
/// counter is owned by `EventStore` (see `counter_next`).
fn store_has_ref(store: &EventStore, r: &str) -> bool {
    store.repo().find_reference(r).is_ok()
}

/// Dispatch a `git forge issue` subcommand. `argv` excludes the `issue` token.
pub fn run_issue(argv: &[String]) -> Result<String, String> {
    let sub = argv.first().map(|s| s.as_str()).unwrap_or("");
    match sub {
        "new" => cmd_new(&argv[1..]),
        "list" => {
            let store = EventStore::open(".").map_err(|e| format!("{e}"))?;
            cmd_list(&store)
        }
        "show" => {
            let store = EventStore::open(".").map_err(|e| format!("{e}"))?;
            cmd_show(&store, &argv[1..])
        }
        "comment" => cmd_comment(&argv[1..]),
        "close" | "reopen" => {
            let kind = if sub == "close" {
                EventKind::IssueClose
            } else {
                EventKind::IssueReopen
            };
            cmd_state(kind, &argv[1..])
        }
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

/// Resolve a `--source`/`--base` argument to its canonical bare local branch
/// name and OID. Accepts `refs/heads/<name>` (full form) and `<name>` (bare
/// form; slashes are legal inside branch names, e.g. `feat/foo`); rejects
/// tags, remote-tracking refs, OIDs, and revision expressions. The returned
/// name is always the bare branch name, so downstream construction of
/// `refs/heads/{name}` (merge, checked-out guard) is always correct.
fn resolve_local_branch(store: &EventStore, arg: &str) -> Result<(String, git2::Oid), String> {
    if arg.is_empty() {
        return Err("branch name must be non-empty".into());
    }
    let (name, full) = if let Some(stripped) = arg.strip_prefix("refs/heads/") {
        // Full form: must be a real refs/heads ref; the canonical name is the
        // stripped remainder (may itself contain slashes, e.g. feat/foo).
        if stripped.is_empty() {
            return Err(format!(
                "'{arg}' is not a canonical local branch; use a plain branch name (tags, remote-tracking refs, OIDs, and revision expressions are rejected)"
            ));
        }
        (stripped.to_string(), arg.to_string())
    } else {
        // Bare form: resolve as refs/heads/<arg>. Slashes are legal inside
        // branch names, so no early '/' rejection — a name that does not
        // resolve is rejected below. `refs/tags/...`, `refs/remotes/...`,
        // OIDs, and revision expressions all fail resolution and are rejected.
        (arg.to_string(), format!("refs/heads/{arg}"))
    };
    let oid = store
        .repo()
        .find_reference(&full)
        .ok()
        .and_then(|r| r.target())
        .ok_or_else(|| {
            format!(
                "'{arg}' is not a canonical local branch; use a plain branch name (tags, remote-tracking refs, OIDs, and revision expressions are rejected)"
            )
        })?;
    Ok((name, oid))
}

/// `git forge pr create --source <branch> --base <branch> <title>`
fn cmd_pr_create(args: &[String]) -> Result<String, String> {
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

    let (store, actor) = open_mutation_store()?;
    let (source, source_oid) = resolve_local_branch(store.store(), &source)?;
    let (base, base_oid) = resolve_local_branch(store.store(), &base)?;
    if source == base {
        return Err("source and base branch must differ (no self-PR)".into());
    }
    if source_oid == base_oid {
        return Err("source and base branches resolve to the same commit (no self-PR)".into());
    }
    let merge_base = crate::git::require_single_merge_base(store.repo(), base_oid, source_oid)?;

    let id = store
        .create_pr(
            title.trim(),
            &source,
            &base,
            source_oid,
            base_oid,
            merge_base,
            &actor,
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
    let bound = store.counter_next().ok().unwrap_or(1);
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
fn cmd_pr_comment(args: &[String]) -> Result<String, String> {
    if args.len() < 2 {
        return Err("usage: git forge pr comment <n> <body>".into());
    }
    let id = parse_entity_id(&args[0])?;
    let body = args[1..].join(" ").trim().to_string();
    if body.is_empty() {
        return Err("comment body must be non-empty".into());
    }
    let (store, actor) = open_mutation_store()?;
    let r = crate::store::pr_head_ref(id);
    if !store_has_ref(store.store(), &r) {
        return Err(format!("PR #{id} does not exist"));
    }
    let ev = Event::new(
        EventKind::PrComment,
        "pr",
        id,
        &actor,
        body_obj(&[("body", json_str(&body))]),
    );
    store.append_event(&r, &ev).map_err(|e| e.to_string())?;
    Ok(format!("comment added to PR #{id}"))
}

/// `git forge pr review <n> --approve|--reject [--file <f> --line <l> --commit <c>]`
fn cmd_pr_review(args: &[String]) -> Result<String, String> {
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
    let (store, actor) = open_mutation_store()?;
    let r = crate::store::pr_head_ref(id);
    if !store_has_ref(store.store(), &r) {
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
    let ev = Event::new(EventKind::PrReview, "pr", id, &actor, body);
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
    let sub = argv.first().map(|s| s.as_str()).unwrap_or("");
    match sub {
        "create" => cmd_pr_create(&argv[1..]),
        "list" => {
            let store = EventStore::open(".").map_err(|e| format!("{e}"))?;
            cmd_pr_list(&store)
        }
        "show" => {
            let store = EventStore::open(".").map_err(|e| format!("{e}"))?;
            cmd_pr_show(&store, &argv[1..])
        }
        "comment" => cmd_pr_comment(&argv[1..]),
        "review" => cmd_pr_review(&argv[1..]),
        "diff" => {
            let store = EventStore::open(".").map_err(|e| format!("{e}"))?;
            cmd_pr_diff(&store, &argv[1..])
        }
        "merge" => {
            let store = EventStore::open(".").map_err(|e| format!("{e}"))?;
            crate::pr_merge::cmd_pr_merge(store, &argv[1..])
        }
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
