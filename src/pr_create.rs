//! `git forge pr create`: branch resolution and atomic PR creation. Split
//! from `cli.rs` to keep it under the size gate.
use crate::cli::{help_requested, help_result, parse_entity_id, store_has_ref};
use crate::cli_error::CliError;
use crate::identity::open_mutation_store;
use crate::machine;
use crate::store::EventStore;

/// Resolve a `--source`/`--base` argument to its canonical bare local branch
/// name and OID. Accepts `refs/heads/<name>` (full form) and `<name>` (bare
/// form; slashes are legal inside branch names, e.g. `feat/foo`); rejects
/// tags, remote-tracking refs, OIDs, and revision expressions. The returned
/// name is always the bare branch name, so downstream construction of
/// `refs/heads/{name}` (merge, checked-out guard) is always correct.
fn resolve_local_branch(store: &EventStore, arg: &str) -> Result<(String, git2::Oid), CliError> {
    if arg.is_empty() {
        return Err(CliError::usage("branch name must be non-empty", ""));
    }
    let name = arg.strip_prefix("refs/heads/").unwrap_or(arg);
    let full = format!("refs/heads/{name}");
    let message = || {
        format!("'{arg}' is not a canonical local branch; use a plain branch name (tags, remote-tracking refs, OIDs, and revision expressions are rejected)")
    };
    if name.is_empty()
        || (arg.starts_with("refs/") && !arg.starts_with("refs/heads/"))
        || !git2::Reference::is_valid_name(&full)
        || name == "HEAD"
    {
        return Err(CliError::invalid(message()));
    }
    let reference = store.repo().find_reference(&full).map_err(|error| {
        let bare_oid =
            arg == name && name.len() == 40 && name.bytes().all(|byte| byte.is_ascii_hexdigit());
        if error.code() == git2::ErrorCode::NotFound && !bare_oid && !name.starts_with('-') {
            CliError::not_found(message(), arg)
        } else {
            CliError::invalid(message())
        }
    })?;
    let oid = reference
        .target()
        .ok_or_else(|| CliError::invalid(message()))?;
    Ok((name.to_string(), oid))
}

/// `git forge pr create --source <branch> --base <branch> <title> [--body <text>] [--label <x>]... [--closes <n>]`
pub(crate) fn cmd_pr_create(args: &[String]) -> Result<String, CliError> {
    const USAGE: &str = "usage: git forge pr create --source <branch> --base <branch> <title> [--body <text>] [--label <x>]... [--closes <n>]";
    // `--json` (#7): the created PR (allocated id, snapshot, pending CI state)
    // as one JSON line — the adapter's id capture.
    let (json, args) = machine::json_requested(args);
    if help_requested(&args) {
        return help_result(json, &args[0], USAGE.into());
    }
    let mut source = None;
    let mut base = None;
    let mut title = None;
    let mut body = None;
    let mut labels: Vec<String> = Vec::new();
    let mut closes = None;
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--" {
            // End-of-options: remaining tokens are the single title positional.
            for t in &args[i + 1..] {
                if title.is_none() {
                    title = Some(t.to_string());
                } else {
                    return Err(CliError::usage("too many positional arguments; usage: git forge pr create --source <branch> --base <branch> <title>", ""));
                }
            }
            break;
        }
        match args[i].as_str() {
            "--source" => {
                i += 1;
                if i >= args.len() {
                    return Err(CliError::usage("--source requires a branch name", ""));
                }
                source = Some(args[i].clone());
            }
            "--base" => {
                i += 1;
                if i >= args.len() {
                    return Err(CliError::usage("--base requires a branch name", ""));
                }
                base = Some(args[i].clone());
            }
            "--body" => {
                i += 1;
                if i >= args.len() {
                    return Err(CliError::usage("--body requires a description", ""));
                }
                body = Some(args[i].trim().to_string());
            }
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
            "--closes" => {
                i += 1;
                if i >= args.len() {
                    return Err(CliError::usage("--closes requires an issue id", ""));
                }
                let id = parse_entity_id(&args[i])?;
                closes = Some(id);
            }
            h @ ("-h" | "--help") => return help_result(json, h, USAGE.into()),
            a if a.starts_with("--") || a.starts_with('-') => {
                return Err(CliError::usage(format!("unknown option '{a}'"), a));
            }
            t if title.is_none() => title = Some(t.to_string()),
            _ => return Err(CliError::usage("too many positional arguments; usage: git forge pr create --source <branch> --base <branch> <title>", "")),
        }
        i += 1;
    }
    let (source, base, title) = source
        .zip(base)
        .zip(title)
        .map(|((source, base), title)| (source, base, title))
        .ok_or_else(|| {
            CliError::usage(
                "usage: git forge pr create --source <branch> --base <branch> <title>",
                "",
            )
        })?;
    if title.trim().is_empty() {
        return Err(CliError::usage(
            "PR title is required and must be non-empty",
            "",
        ));
    }

    let (store, actor) = open_mutation_store()?;
    // `--closes` must name an EXISTING issue; the check runs BEFORE the
    // allocation inside `create_pr`, so a rejected create consumes no counter
    // and creates no PR refs (create remains atomic).
    if let Some(c) = closes {
        let r = crate::store::issue_ref(c);
        if !store_has_ref(store.store(), &r) {
            return Err(CliError::not_found(
                format!("issue #{c} does not exist"),
                c.to_string(),
            ));
        }
    }
    let (source, source_oid) = resolve_local_branch(store.store(), &source)?;
    let (base, base_oid) = resolve_local_branch(store.store(), &base)?;
    if source == base {
        return Err(CliError::invalid(
            "source and base branch must differ (no self-PR)",
        ));
    }
    if source_oid == base_oid {
        return Err(CliError::invalid(
            "source and base branches resolve to the same commit (no self-PR)",
        ));
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
            closes,
        )
        .map_err(CliError::from)?;
    // The pending CI Check marker is published INSIDE `create_pr`'s single
    // atomic store transaction (F-006), so a failure leaves neither the counter
    // nor any PR refs changed — no separate append step can strand a durable PR
    // without its pending marker. No plan executes here (fast + non-destructive);
    // the plan runs on-demand via `git forge ci run <pr>`.
    if json {
        // The allocated PR number is the id the adapter captures; the rest of
        // the snapshot renders from the same values `create_pr` persisted.
        let state = crate::event::PrState {
            id,
            title: Some(title.trim().to_string()),
            description: body.clone().filter(|b| !b.is_empty()),
            labels: labels.clone(),
            source_ref: Some(source.clone()),
            base_ref: Some(base.clone()),
            source_head: Some(source_oid.to_string()),
            base_head: Some(base_oid.to_string()),
            merge_base: Some(merge_base.to_string()),
            closes,
            // create_pr publishes the pending marker atomically (above).
            ci_status: Some("pending".to_string()),
            ci_plan: None,
            ..Default::default()
        };
        return Ok(machine::pr_json(&state));
    }
    Ok(format!("PR #{id} created: {title} ({source} -> {base})"))
}
