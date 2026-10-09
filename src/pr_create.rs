//! `git forge pr create`: branch resolution and atomic PR creation. Split
//! from `cli.rs` to keep it under the size gate.
use crate::cli::{help_requested, help_result, parse_entity_id, store_has_ref};
use crate::cli_error::CliError;
use crate::identity::open_mutation_store;
use crate::machine;
use crate::store::{EventStore, IdempotencyRequest};

/// The refusal for a `--source`/`--base` argument that is not a canonical
/// local branch.
fn not_canonical(arg: &str) -> String {
    format!("'{arg}' is not a canonical local branch; use a plain branch name (tags, remote-tracking refs, OIDs, and revision expressions are rejected)")
}

/// The bare branch name of a `--source`/`--base` argument, checked from the
/// argument alone. Accepts `refs/heads/<name>` (full form) and `<name>` (bare
/// form; slashes are legal inside branch names, e.g. `feat/foo`); rejects
/// other `refs/` namespaces, `HEAD`, and names git does not accept.
fn canonical_branch(arg: &str) -> Result<&str, CliError> {
    let name = arg.strip_prefix("refs/heads/").unwrap_or(arg);
    if name.is_empty()
        || (arg.starts_with("refs/") && !arg.starts_with("refs/heads/"))
        || !git2::Reference::is_valid_name(&format!("refs/heads/{name}"))
        || name == "HEAD"
    {
        return Err(CliError::invalid(not_canonical(arg)));
    }
    Ok(name)
}

/// Resolve a `--source`/`--base` argument to its canonical bare local branch
/// name and OID; rejects tags, remote-tracking refs, OIDs, and revision
/// expressions. The returned name is always the bare branch name, so
/// downstream construction of `refs/heads/{name}` (merge, checked-out guard)
/// is always correct.
fn resolve_local_branch(store: &EventStore, arg: &str) -> Result<(String, git2::Oid), CliError> {
    let name = canonical_branch(arg)?;
    let full = format!("refs/heads/{name}");
    let message = || not_canonical(arg);
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

/// Value-taking options of `pr create` (`--` ends options).
pub(crate) const CREATE_VALUES: &[&str] = &[
    "--source",
    "--base",
    "--body",
    "--label",
    "-l",
    "--closes",
    crate::idempotency::FLAG,
    "--",
];

/// `git forge pr create --source <branch> --base <branch> <title> [--body <text>] [--label <x>]... [--closes <n>] [--idempotency-key <k>]`
pub(crate) fn cmd_pr_create(args: &[String]) -> Result<String, CliError> {
    const USAGE: &str = "usage: git forge pr create --source <branch> --base <branch> <title> [--body <text>] [--label <x>]... [--closes <n>] [--idempotency-key <k>]";
    // `--json` (#7): the created PR (allocated id, snapshot, pending CI state)
    // as one JSON line — the adapter's id capture.
    let (json, args) = machine::json_requested_with(args, CREATE_VALUES);
    if help_requested(&args) {
        return help_result(json, &args[0], USAGE.into());
    }
    let mut source = None;
    let mut base = None;
    let mut title = None;
    let mut body = None;
    let mut labels: Vec<String> = Vec::new();
    let mut closes = None;
    let mut key = None;
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
            crate::idempotency::FLAG => {
                i += 1;
                crate::idempotency::set_key(&mut key, args.get(i))?;
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
    // Pure argument checks, decided before the repository is opened and
    // before an idempotency replay.
    if source.is_empty() || base.is_empty() {
        return Err(CliError::usage("branch name must be non-empty", ""));
    }
    if title.trim().is_empty() {
        return Err(CliError::usage(
            "PR title is required and must be non-empty",
            "",
        ));
    }
    if canonical_branch(&source)? == canonical_branch(&base)? {
        return Err(CliError::invalid(
            "source and base branch must differ (no self-PR)",
        ));
    }
    let description = body.as_deref().filter(|b| !b.is_empty());
    let request = crate::idempotency::pr_create_request(
        key.as_deref(),
        title.trim(),
        (&source, &base),
        description,
        &labels,
        closes,
    )?;
    if let Some(output) = crate::idempotency::replay(request.as_ref(), json)? {
        return Ok(output);
    }

    let input = PrCreateInput {
        source,
        base,
        title,
        description,
        labels,
        closes,
    };
    let created = publish(&input, request.as_ref(), json);
    crate::idempotency::recheck(request.as_ref(), json, created)
}

/// The parsed `pr create` inputs (title untrimmed, as typed).
struct PrCreateInput<'a> {
    source: String,
    base: String,
    title: String,
    description: Option<&'a str>,
    labels: Vec<String>,
    closes: Option<u64>,
}

/// Validate the branches and `--closes`, then create the PR in one store
/// transaction, with `request`'s idempotency record when a key was given.
fn publish(
    input: &PrCreateInput<'_>,
    request: Option<&IdempotencyRequest>,
    json: bool,
) -> Result<String, CliError> {
    let PrCreateInput {
        title,
        description,
        labels,
        closes,
        ..
    } = input;
    let (description, closes) = (*description, *closes);
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
    let (source, source_oid) = resolve_local_branch(store.store(), &input.source)?;
    let (base, base_oid) = resolve_local_branch(store.store(), &input.base)?;
    if source_oid == base_oid {
        return Err(CliError::invalid(
            "source and base branches resolve to the same commit (no self-PR)",
        ));
    }
    let merge_base = crate::git::require_single_merge_base(store.repo(), base_oid, source_oid)?;

    // The pending CI Check marker is published INSIDE `create_pr`'s single
    // atomic store transaction (F-006), so a failure leaves neither the counter
    // nor any PR refs changed — no separate append step can strand a durable PR
    // without its pending marker. No plan executes here (fast + non-destructive);
    // the plan runs on-demand via `git forge ci run <pr>`. The JSON result
    // renders from the same values `create_pr` persisted.
    let render = |id, _| {
        let state = crate::event::PrState {
            id,
            title: Some(title.trim().to_string()),
            description: description.map(str::to_string),
            labels: labels.to_vec(),
            source_ref: Some(source.clone()),
            base_ref: Some(base.clone()),
            source_head: Some(source_oid.to_string()),
            base_head: Some(base_oid.to_string()),
            merge_base: Some(merge_base.to_string()),
            closes,
            ci_status: Some("pending".to_string()),
            ci_plan: None,
            ..Default::default()
        };
        Ok(crate::store::RecordedResult {
            id,
            json: machine::pr_json(&state),
            text: format!("PR #{id} created: {title} ({source} -> {base})"),
        })
    };
    let claim = request.map(|request| crate::store::Claim {
        request,
        render: &render,
    });
    let claimed = store
        .create_pr_claimed(
            title.trim(),
            &source,
            &base,
            source_oid,
            base_oid,
            merge_base,
            &actor,
            description,
            labels,
            closes,
            claim.as_ref(),
        )
        .map_err(CliError::from)?;
    crate::idempotency::output(request, claimed, json, |id| render(id, git2::Oid::zero()))
}
