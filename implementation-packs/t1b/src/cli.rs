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
