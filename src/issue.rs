//! Issue command surface (t1b): `git forge issue` subcommands; `git-issue` dispatches here.
//!
//! All storage comes from t1a's `store`; this module adds command parsing and
//! human-readable output only. Shared CLI plumbing (entity-id parsing, terminal
//! sanitization, store openers, JSON body builders) lives in `crate::cli`.

use std::collections::HashMap;

use crate::cli::{
    body_obj, help_requested, json_str, open_store, parse_entity_id, sanitize_terminal,
    store_has_ref,
};
use crate::event::{Event, EventKind, JsonValue};
use crate::identity::open_mutation_store;
use crate::store::EventStore;

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
    const USAGE: &str = "usage: git forge issue new <title> [description] [--label <x>]...";
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
        return Err(format!("{USAGE} — title is required and non-empty"));
    }
    if positional.len() > 2 {
        // Documented grammar is `<title> [description]`; a third value after
        // `--` (or an over-long positional list) would otherwise be silently
        // discarded.
        return Err(format!("{USAGE} — too many positional arguments"));
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
    const USAGE: &str = "usage: git forge issue comment <n> <body>";
    if args.len() < 2 {
        return Err(USAGE.into());
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

/// Dispatch a `git forge issue` subcommand. `argv` excludes the `issue` token.
pub fn run_issue(argv: &[String]) -> Result<String, String> {
    let sub = argv.first().map(|s| s.as_str()).unwrap_or("");
    // Subcommand-level `-h`/`--help` is answered HERE, before any store open,
    // so help works outside a git repository (same as the namespace helps).
    if help_requested(argv.get(1..).unwrap_or(&[])) {
        if let Some(usage) = issue_sub_usage(sub) {
            return Ok(usage.into());
        }
    }
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

fn issue_sub_usage(sub: &str) -> Option<&'static str> {
    Some(match sub {
        "new" => "usage: git forge issue new <title> [description] [--label <x>]...",
        "list" => "usage: git forge issue list",
        "show" => "usage: git forge issue show <n>",
        "comment" => "usage: git forge issue comment <n> <body>",
        "close" => "usage: git forge issue close <n>",
        "reopen" => "usage: git forge issue reopen <n>",
        _ => return None,
    })
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
