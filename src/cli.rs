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

/// Neutralize terminal control characters in a string about to be rendered to
/// the user's terminal.
///
/// Stored event content (titles, descriptions, labels, comments) is
/// attacker-influenceable and is printed verbatim by `show`/`list`; an
/// embedded ANSI escape (OSC-8/OSC-52, CSI, cursor control) might otherwise
/// execute on display. Every control byte is escaped to its `\xNN` literal —
/// including newline/tab/carriage-return (so a crafted title cannot forge an
/// extra list row or field) and the C1 block U+0080–U+009F (U+009B is CSI on a
/// UTF-8 virtual console).
///
/// Visible text is preserved; nothing can move the cursor, clear the screen, or
/// emit a terminal action.
fn sanitize_terminal(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        let cp = c as u32;
        if cp < 0x20 || (0x7F..=0x9F).contains(&cp) {
            out.push_str(&format!("\\x{:02x}", cp));
        } else {
            out.push(c);
        }
    }
    out
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
    // later config-open failure (error precedence, F-028). Supports
    // `--label <x>` (repeatable) alongside the positional `<title> [description]`.
    let mut labels: Vec<String> = Vec::new();
    let mut positional: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--" {
            // End-of-options: everything after `--` is positional, so a title
            // that exactly equals a reserved flag name (`-l`, `--label`) is
            // representable.
            positional.extend_from_slice(&args[i + 1..]);
            break;
        }
        match args[i].as_str() {
            "--label" | "-l" => {
                i += 1;
                if i >= args.len() {
                    return Err("--label requires a value".into());
                }
                let v = args[i].trim().to_string();
                if v.is_empty() {
                    return Err("--label requires a non-empty value".into());
                }
                labels.push(v);
            }
            _ => positional.push(args[i].clone()),
        }
        i += 1;
    }
    if positional.is_empty() || positional[0].trim().is_empty() {
        return Err(
            "usage: git forge issue new <title> [description] [--label <x>]... — title is required and non-empty"
                .into(),
        );
    }
    if positional.len() > 2 {
        // Documented grammar is `<title> [description]`; a third value after
        // `--` (or an over-long positional list) would otherwise be silently
        // discarded.
        return Err(
            "usage: git forge issue new <title> [description] [--label <x>]... — too many positional arguments"
                .into(),
        );
    }
    let title = positional[0].trim().to_string();
    let description = positional
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
    if !labels.is_empty() {
        body.insert("labels".into(), crate::event::json_string_array(&labels));
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
            sanitize_terminal(state.title.as_deref().unwrap_or("(untitled)")),
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
        sanitize_terminal(state.title.as_deref().unwrap_or("(untitled)")),
        if state.open { "open" } else { "closed" }
    );
    if let Some(d) = &state.description {
        out.push_str(&format!("description: {}\n", sanitize_terminal(d)));
    }
    if !state.labels.is_empty() {
        out.push_str(&format!(
            "labels: {}\n",
            sanitize_terminal(&state.labels.join(", "))
        ));
    }
    out.push('\n');
    if !state.comments.is_empty() {
        out.push_str("comments:\n");
        for c in &state.comments {
            out.push_str(&format!("  - {}\n", sanitize_terminal(c)));
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

/// Open the workspace store for a READ command (read-only command surface).
fn open_store() -> Result<EventStore, String> {
    EventStore::open(".").map_err(|e| format!("{e}"))
}

/// Dispatch a `git forge issue` subcommand. `argv` excludes the `issue` token.
pub fn run_issue(argv: &[String]) -> Result<String, String> {
    let sub = argv.first().map(|s| s.as_str()).unwrap_or("");
    match sub {
        "new" => cmd_new(&argv[1..]),
        "list" => cmd_list(&open_store()?),
        "show" => cmd_show(&open_store()?, &argv[1..]),
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
     \x20 new <title> [description] [--label <x>]...\n\
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

/// `git forge pr create --source <branch> --base <branch> <title> [--body <text>] [--label <x>]...`
fn cmd_pr_create(args: &[String]) -> Result<String, String> {
    let mut source = None;
    let mut base = None;
    let mut title = None;
    let mut body = None;
    let mut labels: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--" {
            // End-of-options: remaining tokens are the single title positional.
            for t in &args[i + 1..] {
                if title.is_none() {
                    title = Some(t.to_string());
                } else {
                    return Err("too many positional arguments; usage: git forge pr create --source <branch> --base <branch> <title>".into());
                }
            }
            break;
        }
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
            "--body" => {
                i += 1;
                if i >= args.len() {
                    return Err("--body requires a description".into());
                }
                body = Some(args[i].trim().to_string());
            }
            "--label" | "-l" => {
                i += 1;
                if i >= args.len() {
                    return Err("--label requires a value".into());
                }
                let v = args[i].trim().to_string();
                if v.is_empty() {
                    return Err("--label requires a non-empty value".into());
                }
                labels.push(v);
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
            body.as_deref().filter(|b| !b.is_empty()),
            &labels,
        )
        .map_err(|e| e.to_string())?;
    // Record a pending CI Check marker: no plan executes here (fast +
    // non-destructive); the plan runs on-demand via `git forge ci run <pr>`.
    let mut ci_body = HashMap::new();
    ci_body.insert("status".into(), json_str("pending"));
    let ci_ev = Event::new(EventKind::CiCheck, "pr", id, &actor, ci_body);
    store
        .append_event(&crate::store::pr_head_ref(id), &ci_ev)
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
        sanitize_terminal(state.title.as_deref().unwrap_or("(untitled)")),
        state
            .effective_decision
            .as_deref()
            .map(|d| format!("decision: {}", sanitize_terminal(d)))
            .unwrap_or_else(|| "no review yet".into())
    );
    if let Some(r) = &state.merge_result {
        out.push_str(&format!("merged: {}\n", sanitize_terminal(r)));
    }
    // The latest CI Check (fold) is surfaced so the run's outcome is readable.
    if let Some(s) = &state.ci_status {
        match &state.ci_plan {
            Some(p) => out.push_str(&format!(
                "ci: {} ({})\n",
                sanitize_terminal(s),
                sanitize_terminal(p)
            )),
            None => out.push_str(&format!("ci: {}\n", sanitize_terminal(s))),
        }
    }
    if let Some(r) = &state.base_ref {
        out.push_str(&format!("base: {}\n", sanitize_terminal(r)));
    }
    if let Some(r) = &state.source_ref {
        out.push_str(&format!("source: {}\n", sanitize_terminal(r)));
    }
    if let (Some(b), Some(s)) = (&state.base_head, &state.source_head) {
        out.push_str(&format!(
            "diff: {}...{}\n",
            sanitize_terminal(b),
            sanitize_terminal(s)
        ));
    }
    if let Some(d) = &state.description {
        out.push_str(&format!("description: {}\n", sanitize_terminal(d)));
    }
    if !state.labels.is_empty() {
        out.push_str(&format!(
            "labels: {}\n",
            sanitize_terminal(&state.labels.join(", "))
        ));
    }
    for c in &state.comments {
        out.push_str(&format!("comment: {}\n", sanitize_terminal(c)));
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
            sanitize_terminal(st.title.as_deref().unwrap_or("(untitled)")),
            sanitize_terminal(st.effective_decision.as_deref().unwrap_or("no review"))
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
        "list" => cmd_pr_list(&open_store()?),
        "show" => cmd_pr_show(&open_store()?, &argv[1..]),
        "comment" => cmd_pr_comment(&argv[1..]),
        "review" => cmd_pr_review(&argv[1..]),
        "diff" => cmd_pr_diff(&open_store()?, &argv[1..]),
        "merge" => crate::pr_merge::cmd_pr_merge(open_store()?, &argv[1..]),
        "help" | "-h" | "--help" => Ok(pr_help()),
        "" => Ok(pr_help()),
        other => Err(format!("unknown pr subcommand '{other}'")),
    }
}

fn pr_help() -> String {
    "usage: git forge pr <create|list|show|comment|review|diff|merge> ...\n\
     \nsubcommands:\n\
     \x20 create --source <branch> --base <branch> <title> [--body <text>] [--label <x>]...\n\
     \x20 list\n\
     \x20 show <n>\n\
     \x20 comment <n> <body>\n\
     \x20 review <n> --approve|--reject [--file <f> --line <l> --commit <c>]\n\
     \x20 diff <n>\n\
     \x20 merge [<n>] [--squash|--rebase]"
        .to_string()
}

// ─────────────────────────── CI commands (t0) ───────────────────────────

/// Run the repo CI plan inside the temp worktree (the PR's own commits) and
/// return its exit status (`None` when the process could not be spawned).
/// `.forge/ci.sh` is executed with `bash` (respects the script's own shell
/// semantics); the `just check` fallback runs the `just` recipe directly.
/// Both run with the worktree as the working directory, so they validate
/// exactly the PR tree.
fn run_ci_plan(worktree: &std::path::Path, plan: &str) -> Option<i32> {
    use std::process::Command;
    let (prog, args): (&str, &[&str]) = if plan == "just check" {
        ("just", &["check"])
    } else {
        ("bash", &[".forge/ci.sh"])
    };
    let out = Command::new(prog)
        .args(args)
        .current_dir(worktree)
        .output()
        .ok()?;
    out.status.code()
}

/// `git forge ci run <pr>` — execute the repo CI plan against the PR's own
/// commits in a temporary worktree, append a CI Check event recording the
/// outcome, and leave the developer's working tree and current branch
/// untouched. The plan is `.forge/ci.sh` when present in the PR tree,
/// otherwise the `just check` fallback.
fn cmd_ci_run(args: &[String]) -> Result<String, String> {
    let id = parse_entity_id(args.first().map(|s| s.as_str()).unwrap_or(""))?;
    let (store, actor) = open_mutation_store()?;
    let head = crate::store::pr_head_ref(id);
    if !store_has_ref(store.store(), &head) {
        return Err(format!("PR #{id} does not exist"));
    }
    // The plan runs against the PR's immutable source snapshot (its own
    // commits), never the developer's working tree.
    let source_oid = store
        .repo()
        .find_reference(&crate::store::pr_source_ref(id))
        .and_then(|r| r.target().ok_or(git2::Error::from_str("no target")))
        .map_err(|_| format!("PR #{id} snapshot source ref missing"))?;
    let repo_dir = store
        .repo()
        .workdir()
        .map(|p| p.to_path_buf())
        .ok_or_else(|| "cannot resolve repository working directory".to_string())?;

    // Reserve a unique temp worktree path (sibling lock) so concurrent CI runs
    // on the same repo never collide on the global temp dir.
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    repo_dir.to_string_lossy().as_bytes().hash(&mut hasher);
    let nonce = hasher.finish();
    let mk = |attempt: u32| {
        std::env::temp_dir().join(format!(
            "git-forge-pr{id}-ci-{nonce:x}-{}-{attempt}",
            std::process::id()
        ))
    };
    let mut tmp = mk(0);
    let mut attempt = 0u32;
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
                match crate::git::worktree_add(&repo_dir, &tmp, &source_oid) {
                    Ok(()) => break (true, String::new()),
                    Err(err) => break (false, err),
                }
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
        drop(lock_handle.take());
        let _ = std::fs::remove_file(tmp.with_extension("lock"));
        return Err(format!(
            "failed to create temporary worktree for CI run: {err_wt}"
        ));
    }
    // Release the path lock now that the worktree is registered; every early
    // return after this point must drop it too.
    let release_lock = |lock_handle: &mut Option<std::fs::File>, path: &std::path::Path| {
        *lock_handle = None;
        let _ = std::fs::remove_file(path.with_extension("lock"));
    };

    // Determine the plan from the PR tree itself: `.forge/ci.sh` when present,
    // otherwise the `just check` fallback.
    let plan = if tmp.join(".forge").join("ci.sh").exists() {
        ".forge/ci.sh".to_string()
    } else {
        "just check".to_string()
    };

    // Run the plan. The exit status is captured; the plan's stderr/stdout stay
    // in the worktree (not surfaced), only the recorded status matters here.
    let script_status = run_ci_plan(&tmp, &plan);

    // Always append the CI Check outcome — a failing plan must still record a
    // `failure` CI Check (VAL-002) before the command exits nonzero.
    let status = if script_status == Some(0) {
        "success"
    } else {
        "failed"
    };
    let mut body = HashMap::new();
    body.insert("status".into(), json_str(status));
    body.insert("plan".into(), json_str(&plan));
    let ev = Event::new(EventKind::CiCheck, "pr", id, &actor, body);
    // Record the outcome — a failing plan still appends a `failure` CI Check
    // (VAL-002) before the command exits nonzero.
    let append = store.append_event(&head, &ev);

    // Clean up the temp worktree regardless of the plan/append outcome,
    // holding the sibling lock until the worktree is removed AND verified
    // gone (directory absent + path unregistered), mirroring the merge-path
    // postconditions (F-002/F-003): a leftover is reported, never silently
    // discarded, and success is never claimed over a still-present worktree.
    let leftover = crate::git::worktree_remove(&repo_dir, &tmp)
        .err()
        .map(|e| format!("temp worktree removal failed: {e}"))
        .or_else(|| {
            let (ok, list, err_l) = crate::git::worktree_list_raw(&repo_dir);
            if tmp.exists() {
                Some("temp worktree directory still exists".to_string())
            } else if !ok {
                Some(format!("worktree verification failed: {err_l}"))
            } else if crate::event::worktree_registered_path(&list, &tmp) {
                Some("temp worktree still registered".to_string())
            } else {
                None
            }
        })
        .map(|reason| format!("{reason} (worktree left at {})", tmp.display()));
    release_lock(&mut lock_handle, &tmp);

    if let Some(leftover) = leftover {
        let prefix = match append.as_ref().err() {
            Some(ar) => format!("CI run completed but recording the CI Check failed: {ar};"),
            None => format!("CI run recorded {status} but"),
        };
        return Err(format!("{prefix} temp worktree cleanup failed: {leftover}"));
    }
    append.map_err(|e| format!("CI run completed but recording the CI Check failed: {e}"))?;

    if script_status == Some(0) {
        Ok(format!("CI run for PR #{id} passed ({plan})"))
    } else {
        Err(format!(
            "CI run for PR #{id} failed ({plan}){}",
            script_status
                .map(|c| format!(": exited with status {c}"))
                .unwrap_or_default()
        ))
    }
}

/// Dispatch a `git forge ci` subcommand. `argv` excludes the `ci` token.
pub fn run_ci(argv: &[String]) -> Result<String, String> {
    let sub = argv.first().map(|s| s.as_str()).unwrap_or("");
    match sub {
        "run" => cmd_ci_run(&argv[1..]),
        "help" | "-h" | "--help" => Ok(ci_help()),
        "" => Ok(ci_help()),
        other => Err(format!("unknown ci subcommand '{other}'")),
    }
}

fn ci_help() -> String {
    "usage: git forge ci <run> ...\n\
     \nsubcommands:\n\
     \x20 run <n>\n\
     \x20 run `git forge ci run <pr>` to execute the repo CI plan and record a CI Check"
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::sanitize_terminal;

    /// Terminal control sequences are neutralized to their `\xNN` literal while
    /// visible text (including non-ASCII letters) survives.
    #[test]
    fn sanitize_terminal_neutralizes_control_sequences() {
        // OSC-8 hyperlink / ESC cursor sequence must not survive verbatim.
        let malicious = "\u{001b}]8;;http://evil.example\u{0007}click\u{001b}]8;;\u{0007}";
        let out = sanitize_terminal(malicious);
        assert!(!out.contains('\u{001b}'), "ESC must be escaped: {out:?}");
        assert!(!out.contains('\u{0007}'), "BEL must be escaped: {out:?}");
        assert!(out.contains("\\x1b"), "ESC rendered as \\x1b: {out:?}");
        assert!(out.contains("click"), "visible text preserved: {out:?}");
        // Structural whitespace (newline/tab/carriage-return) is escaped so a
        // stored title cannot forge an extra list row or field.
        assert_eq!(sanitize_terminal("a\rb"), "a\\x0db");
        assert_eq!(
            sanitize_terminal("line1\nline2\tend"),
            "line1\\x0aline2\\x09end"
        );
        // DEL and C1 controls (U+0080-U+009F; U+009B is CSI) are escaped.
        assert_eq!(sanitize_terminal("caf\u{7f}"), "caf\\x7f");
        assert_eq!(sanitize_terminal("x\u{009b}2J"), "x\\x9b2J");
        // Plain printable and non-ASCII visible text survive.
        assert_eq!(sanitize_terminal("plain text"), "plain text");
        assert_eq!(sanitize_terminal("café 中文 🚀"), "café 中文 🚀");
    }
}
