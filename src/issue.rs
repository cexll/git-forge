//! Issue command surface (t1b): `git forge issue` subcommands; `git-issue` dispatches here.
//!
//! All storage comes from t1a's `store`; this module adds command parsing and
//! human-readable output only. Shared CLI plumbing (entity-id parsing, terminal
//! sanitization, store openers, JSON body builders) lives in `crate::cli`.

use crate::cli_error::CliError;

use crate::cli::{
    body_obj, help_requested, help_result, json_str, open_store, parse_entity_id,
    sanitize_terminal, store_has_ref,
};
use crate::event::{Event, EventKind, JsonValue};
use crate::identity::open_mutation_store;
use crate::machine;
use std::collections::HashMap;

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

/// Value-taking options of `issue new` (`--` ends options).
const NEW_VALUES: &[&str] = &[
    "--label",
    "-l",
    "--parent",
    "--blocked-by",
    crate::idempotency::FLAG,
    "--",
];
/// Value-taking options of `issue list`.
const LIST_VALUES: &[&str] = &["--parent", "--state"];
/// Value-taking options of `issue comment` and `pr comment`.
pub(crate) const COMMENT_VALUES: &[&str] = &[crate::idempotency::FLAG];

/// The value-taking options of issue subcommand `sub`.
fn issue_values(sub: &str) -> &'static [&'static str] {
    match sub {
        "new" => NEW_VALUES,
        "list" => LIST_VALUES,
        "comment" => COMMENT_VALUES,
        _ => &[],
    }
}

/// `git forge issue new <title> [description]`
fn cmd_new(args: &[String]) -> Result<String, CliError> {
    const USAGE: &str = "usage: git forge issue new <title> [description] [--label <x>]... [--parent <n>] [--blocked-by <n>]... [--idempotency-key <k>] [--json]";
    // Pure argument validation first — a usage error must not be masked by a
    // later config-open failure (error precedence, F-028). Supports
    // `--label <x>` (repeatable) alongside the positional `<title> [description]`.
    let (json, args) = machine::json_requested_with(args, NEW_VALUES);
    let mut labels: Vec<String> = Vec::new();
    let mut parent = None;
    let mut blocked_by = Vec::new();
    let mut positional: Vec<String> = Vec::new();
    let mut key = None;
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
                    return Err(CliError::usage("--label requires a value", ""));
                }
                let v = args[i].trim().to_string();
                if v.is_empty() {
                    return Err(CliError::usage("--label requires a non-empty value", ""));
                }
                labels.push(v);
            }
            "--parent" | "--blocked-by" => {
                let flag = args[i].as_str();
                i += 1;
                let value = args
                    .get(i)
                    .ok_or_else(|| CliError::usage(format!("{flag} requires an issue id"), flag))?;
                let id = parse_entity_id(value)?;
                if flag == "--parent" {
                    if parent.replace(id).is_some() {
                        return Err(CliError::usage("--parent may only be specified once", ""));
                    }
                } else {
                    blocked_by.push(id);
                }
            }
            crate::idempotency::FLAG => {
                i += 1;
                crate::idempotency::set_key(&mut key, args.get(i))?;
            }
            a if a.starts_with('-') => {
                return Err(CliError::usage(format!("unknown option '{a}'; {USAGE}"), a))
            }
            _ => positional.push(args[i].clone()),
        }
        i += 1;
    }
    if positional.is_empty() || positional[0].trim().is_empty() {
        return Err(CliError::usage(
            format!("{USAGE} — title is required and non-empty"),
            "",
        ));
    }
    if positional.len() > 2 {
        // Documented grammar is `<title> [description]`; a third value after
        // `--` (or an over-long positional list) would otherwise be silently
        // discarded.
        return Err(CliError::usage(
            format!("{USAGE} — too many positional arguments"),
            "",
        ));
    }
    let title = positional[0].trim().to_string();
    let description = positional
        .get(1)
        .map(|d| d.trim().to_string())
        .filter(|d| !d.is_empty());
    let request = crate::idempotency::issue_new_request(
        key.as_deref(),
        &title,
        description.as_deref(),
        &labels,
        parent,
        &blocked_by,
    )?;
    if let Some(output) = crate::idempotency::replay(request.as_ref(), json)? {
        return Ok(output);
    }
    let fields = (title, description, labels, parent, blocked_by);
    let created = crate::issue_apply::new_issue_keyed(fields, json, request.as_ref());
    crate::idempotency::recheck(request.as_ref(), json, created)
}

/// `git forge issue list`
fn cmd_list(args: &[String]) -> Result<String, CliError> {
    const USAGE: &str =
        "usage: git forge issue list [--parent <n>] [--state open|closed|all] [--json]";
    let (json, args) = machine::json_requested_with(args, LIST_VALUES);
    let mut parent = None;
    let mut desired_state = "all";
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--parent" => {
                i += 1;
                let value = args
                    .get(i)
                    .ok_or_else(|| CliError::usage("--parent requires an issue id", "--parent"))?;
                if parent.replace(parse_entity_id(value)?).is_some() {
                    return Err(CliError::usage("--parent may only be specified once", ""));
                }
            }
            "--state" => {
                i += 1;
                desired_state = args.get(i).map(String::as_str).ok_or_else(|| {
                    CliError::usage("--state requires open, closed, or all", "--state")
                })?;
                if !matches!(desired_state, "open" | "closed" | "all") {
                    return Err(CliError::usage(
                        format!("invalid --state '{desired_state}'; expected open, closed, or all"),
                        desired_state,
                    ));
                }
            }
            _ => machine::reject_leftover_flags(&args[i..=i], USAGE)?,
        }
        i += 1;
    }
    let store = open_store()?;
    let all = machine::fold_all_issues(&store)?;
    let states: Vec<_> = all
        .iter()
        .filter(|state| {
            parent.is_none_or(|id| state.parent == Some(id))
                && match desired_state {
                    "open" => state.open,
                    "closed" => !state.open,
                    _ => true,
                }
        })
        .collect();
    if json {
        return Ok(machine::issue_list_json(&states, &all));
    }
    let mut out = String::new();
    for state in &states {
        out.push_str(&format!(
            "#{} {} ({})\n",
            state.id,
            sanitize_terminal(state.title.as_deref().unwrap_or("(untitled)")),
            if state.open { "open" } else { "closed" }
        ));
    }
    if states.is_empty() {
        return Ok("(no issues)".to_string());
    }
    Ok(out.trim_end().to_string())
}

/// `git forge issue show <n>`
fn cmd_show(args: &[String]) -> Result<String, CliError> {
    let (json, args) = machine::json_requested(args);
    machine::reject_leftover_flags(
        args.get(1..).unwrap_or(&[]),
        "usage: git forge issue show <n>",
    )?;
    let id = parse_entity_id(
        args.first()
            .ok_or_else(|| CliError::usage("usage: git forge issue show <n> [--json]", ""))?,
    )?;
    let store = open_store()?;
    let r = crate::store::issue_ref(id);
    if !store_has_ref(&store, &r) {
        return Err(CliError::not_found(
            format!("issue #{id} does not exist"),
            id.to_string(),
        ));
    }
    let chain = store.read_chain(&r).map_err(CliError::from)?;
    if chain.is_empty() {
        return Err((format!("issue #{id} has no events")).into());
    }
    let state = crate::event::fold(&chain).issue;
    let all = machine::fold_all_issues(&store)?;
    if json {
        return Ok(machine::issue_json(&state, &all));
    }
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
    let children: Vec<_> = all
        .iter()
        .filter(|issue| issue.parent == Some(state.id))
        .map(|issue| format!("#{}", issue.id))
        .collect();
    if !children.is_empty() {
        out.push_str(&format!("children: {}\n", children.join(", ")));
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
fn cmd_comment(args: &[String]) -> Result<String, CliError> {
    const USAGE: &str = "usage: git forge issue comment <n> <body>";
    // Same strict flag/token split as every other issue subcommand: an
    // unknown option is a clean usage error naming the flag (VAL-202),
    // never a silently-ignored prefix of the comment body.
    let (json, args) = machine::json_requested_with(args, COMMENT_VALUES);
    let (key, args) = crate::idempotency::take_key(&args)?;
    // The grammar is `<n> <body>` — positionals ARE the body. Only flag
    // tokens are rejected here (naming the flag, VAL-202); existence and
    // emptiness are checked below, as before.
    machine::reject_flags(&args, USAGE)?;
    if args.len() < 2 {
        return Err(CliError::usage(USAGE, ""));
    }
    let id = parse_entity_id(&args[0])?;
    let body = args[1..].join(" ").trim().to_string();
    if body.is_empty() {
        return Err(CliError::usage("comment body must be non-empty", ""));
    }
    let kind = crate::store::IdempotentCommand::IssueComment;
    let request = crate::idempotency::comment_request(kind, key.as_deref(), id, &body)?;
    if let Some(output) = crate::idempotency::replay(request.as_ref(), json)? {
        return Ok(output);
    }
    let r = crate::store::issue_ref(id);
    let ev = |actor: &str| {
        issue_event(
            EventKind::IssueComment,
            id,
            actor,
            body_obj(&[("body", json_str(&body))]),
        )
    };
    if let Some(request) = &request {
        let state = |folded: &crate::event::FoldState| machine::issue_state_name(&folded.issue);
        return crate::idempotency::comment(&r, id, &ev, request, json, state);
    }
    let (store, actor) = open_mutation_store()?;
    if !store_has_ref(store.store(), &r) {
        return Err(CliError::not_found(
            format!("issue #{id} does not exist"),
            id.to_string(),
        ));
    }
    let ev = ev(&actor);
    let event = store.append_event(&r, &ev).map_err(CliError::from)?;
    if json {
        let state = machine::issue_state_at(store.store(), event)?;
        return Ok(machine::event_result_json(id, state, &event.to_string()));
    }
    Ok(format!("comment added to issue #{id}"))
}

/// `git forge issue close <n>` / `reopen <n>`
fn cmd_state(kind: EventKind, args: &[String]) -> Result<String, CliError> {
    // One positional, no options: anything dash-leading is a usage error
    // naming the flag (VAL-202).
    let (json, args) = machine::json_requested(args);
    let verb = match kind {
        EventKind::IssueClose => "close",
        EventKind::IssueReopen => "reopen",
        _ => unreachable!(),
    };
    // Flag tokens first (naming the flag, VAL-202); arity is a plain usage
    // error. One positional, no options.
    machine::reject_flags(&args, &format!("usage: git forge issue {verb} <n>"))?;
    if args.len() != 1 {
        return Err(CliError::usage(
            format!("usage: git forge issue {verb} <n>"),
            "",
        ));
    }
    let id = parse_entity_id(args.first().map(|s| s.as_str()).unwrap_or(""))?;
    let (store, actor) = open_mutation_store()?;
    let r = crate::store::issue_ref(id);
    if !store_has_ref(store.store(), &r) {
        return Err(CliError::not_found(
            format!("issue #{id} does not exist"),
            id.to_string(),
        ));
    }
    let ev = issue_event(kind, id, &actor, HashMap::new());
    let event = store.append_event(&r, &ev).map_err(CliError::from)?;
    let verb = match kind {
        EventKind::IssueClose => "closed",
        EventKind::IssueReopen => "reopened",
        _ => unreachable!(),
    };
    if json {
        let state = machine::issue_state_at(store.store(), event)?;
        return Ok(machine::event_result_json(id, state, &event.to_string()));
    }
    Ok(format!("issue #{id} {verb}"))
}

/// Dispatch a `git forge issue` subcommand. `argv` excludes the `issue` token.
pub fn run_issue(argv: &[String]) -> Result<String, CliError> {
    let sub = argv.first().map(|s| s.as_str()).unwrap_or("");
    // Subcommand-level `-h`/`--help` is answered HERE, before any store open,
    // so help works outside a git repository (same as the namespace helps).
    let (json, rest) = machine::json_requested_by_dispatch(argv, issue_values(sub));
    if help_requested(&rest) {
        if let Some(usage) = issue_sub_usage(sub) {
            return help_result(json, &rest[0], usage.into());
        }
    }
    match sub {
        "new" => cmd_new(&argv[1..]),
        "apply" => crate::issue_apply::cmd_apply(&argv[1..]).map_err(|mut error| {
            error.message = sanitize_terminal(&error.message);
            error
        }),
        "list" => cmd_list(&argv[1..]),
        "show" => cmd_show(&argv[1..]),
        "comment" => cmd_comment(&argv[1..]),
        "close" | "reopen" => {
            let kind = if sub == "close" {
                EventKind::IssueClose
            } else {
                EventKind::IssueReopen
            };
            cmd_state(kind, &argv[1..])
        }
        "help" | "-h" | "--help" => help_result(json, sub, issue_help()),
        "" => Ok(issue_help()),
        other => Err(CliError::usage(
            format!("unknown issue subcommand '{other}'"),
            other,
        )),
    }
}

fn issue_sub_usage(sub: &str) -> Option<&'static str> {
    Some(match sub {
        "new" => "usage: git forge issue new <title> [description] [--label <x>]... [--parent <n>] [--blocked-by <n>]... [--idempotency-key <k>] [--json]",
        "apply" => "usage: git forge issue apply <dir|file> [--dry-run] [--json]",
        "list" => "usage: git forge issue list [--parent <n>] [--state open|closed|all] [--json]",
        "show" => "usage: git forge issue show <n>",
        "comment" => "usage: git forge issue comment <n> <body> [--idempotency-key <k>] [--json]",
        "close" => "usage: git forge issue close <n> [--json]",
        "reopen" => "usage: git forge issue reopen <n> [--json]",
        _ => return None,
    })
}

fn issue_help() -> String {
    "usage: git forge issue <new|apply|list|show|comment|close|reopen> ...\n\
     \nsubcommands:\n\
     \x20 new <title> [description] [--label <x>]... [--parent <n>] [--blocked-by <n>]... [--idempotency-key <k>] [--json]\n\
     \x20 apply <dir|file> [--dry-run] [--json]\n\
     \x20 list [--parent <n>] [--state open|closed|all] [--json]\n\
     \x20 show <n> [--json]\n\
     \x20 comment <n> <body> [--idempotency-key <k>] [--json]\n\
     \x20 close <n> [--json]\n\
     \x20 reopen <n> [--json]"
        .to_string()
}
