//! CLI layer: shared command plumbing plus the `git forge pr` / `git forge ci`
//! surfaces. Issue commands live in `crate::issue` (t1b); the merge gate and
//! orchestration live in `crate::pr_merge`; CI plan execution in `crate::ci`.

use crate::cli_error::CliError;

use std::collections::HashMap;

use crate::event::{Event, EventKind, JsonValue};
use crate::identity::open_mutation_store;
use crate::machine;
use crate::store::EventStore;

/// Parse `"<n>"` into a positive u64, else a clean usage error (no panic).
pub(crate) fn parse_entity_id(s: &str) -> Result<u64, CliError> {
    if s.is_empty() {
        return Err(CliError::usage("empty entity id".to_string(), ""));
    }
    let n: u64 = s.parse().map_err(|_| {
        CliError::usage(
            format!("invalid entity id '{s}' (expected a positive integer)"),
            s,
        )
    })?;
    if n == 0 {
        return Err(CliError::usage("entity id must be positive", s));
    }
    Ok(n)
}

/// True when the first argument is `-h`/`--help`: a leading help flag prints
/// the subcommand's usage with a success exit — the convention set by
/// `pr merge`. Without this the flag fell into entity-id parsing
/// ("invalid entity id '--help'") or, for `issue new`, became an issue title.
pub(crate) fn help_requested(args: &[String]) -> bool {
    matches!(
        args.first().map(String::as_str),
        Some("-h") | Some("--help")
    )
}

/// The help arm of a command parser, reached only where the parser reads
/// `token` as a help request (never as an option value). Help has no JSON
/// result, so under `--json` it is an `E_USAGE` failure, not prose on stdout.
pub fn help_result(json: bool, token: &str, usage: String) -> Result<String, CliError> {
    if json {
        return Err(json_help_refused(token));
    }
    Ok(usage)
}

/// The `E_USAGE` failure for a help request made under `--json`.
pub(crate) fn json_help_refused(token: &str) -> CliError {
    CliError::usage(
        "--json cannot be combined with a help request; help has no JSON result",
        token,
    )
}

pub(crate) fn json_str(value: &str) -> JsonValue {
    JsonValue::String(value.to_string())
}

pub(crate) fn body_obj(pairs: &[(&str, JsonValue)]) -> HashMap<String, JsonValue> {
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
pub(crate) fn sanitize_terminal(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        let cp = c as u32;
        if cp < 0x20 || (0x7F..=0x9F).contains(&cp) {
            out.push_str(&format!("\\x{cp:02x}"));
        } else {
            out.push(c);
        }
    }
    out
}

/// True if the given ref exists in this repo (store-level existence check,
/// used by issue/PR commands before operating). Not a counter read — the
/// counter is owned by `EventStore` (see `counter_next`).
pub(crate) fn store_has_ref(store: &EventStore, r: &str) -> bool {
    store.repo().find_reference(r).is_ok()
}

/// Open the workspace store for a READ command (read-only command surface).
pub(crate) fn open_store() -> Result<EventStore, CliError> {
    EventStore::open(".").map_err(CliError::from)
}

// ─────────────────────────── PR commands ───────────────────────────

/// `git forge pr show <n>`
fn cmd_pr_show(args: &[String]) -> Result<String, CliError> {
    let (json, args) = machine::json_requested(args);
    let (id_arg, rest) = args
        .split_first()
        .ok_or_else(|| CliError::usage("usage: git forge pr show <n> [--json]", ""))?;
    machine::reject_leftover_flags(rest, "usage: git forge pr show <n>")?;
    let id = parse_entity_id(id_arg)?;
    let store = open_store()?;
    let r = crate::store::pr_head_ref(id);
    if !store_has_ref(&store, &r) {
        return Err(CliError::not_found(
            format!("PR #{id} does not exist"),
            id_arg,
        ));
    }
    let chain = store.read_chain(&r).map_err(CliError::from)?;
    if chain.is_empty() {
        return Err(format!("PR #{id} has no events").into());
    }
    let state = crate::event::fold(&chain).pr;
    if json {
        return Ok(machine::pr_json(&state));
    }
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
    // t8 (#8): role attribution, additive lines (absent on roleless chains,
    // so prose parsers and the single-user default see no change).
    if let Some(r) = &state.creator_role {
        out.push_str(&format!("created-by: {}\n", sanitize_terminal(r)));
    }
    if let (Some(d), Some(r)) = (&state.effective_decision, &state.decision_role) {
        out.push_str(&format!(
            "decision-by: {} ({})\n",
            sanitize_terminal(r),
            sanitize_terminal(d)
        ));
    }
    if let Some(reason) = &state.abandoned {
        out.push_str(&format!("abandoned: {}\n", sanitize_terminal(reason)));
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
    // t1: the PR's current snapshot generation (0 = the authoritative
    // pr.created snapshot, +1 per pr.update).
    out.push_str(&format!("generation: {}\n", state.generation));
    // F-002: the latest prepared candidate, additive (safe for adapter
    // prose parsers: one extra line, no existing field reshaped).
    if let Some(c) = &state.prepared {
        out.push_str(&format!(
            "prepared: strategy {} result {}\n",
            sanitize_terminal(&c.strategy),
            sanitize_terminal(&c.result_commit)
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
    if let Some(c) = &state.closes {
        out.push_str(&format!("closes: #{c}\n"));
    }
    for c in &state.comments {
        out.push_str(&format!("comment: {}\n", sanitize_terminal(c)));
    }
    Ok(out.trim_end().to_string())
}

/// `git forge pr list`
fn cmd_pr_list(args: &[String]) -> Result<String, CliError> {
    let (json, args) = machine::json_requested(args);
    machine::reject_leftover_flags(&args, "usage: git forge pr list")?;
    let states = machine::fold_all_prs(&open_store()?)?;
    if json {
        return Ok(machine::pr_list_json(&states));
    }
    let mut out = String::new();
    for st in &states {
        out.push_str(&format!(
            "PR #{} {} ({})\n",
            st.id,
            sanitize_terminal(st.title.as_deref().unwrap_or("(untitled)")),
            sanitize_terminal(st.effective_decision.as_deref().unwrap_or("no review"))
        ));
    }
    if states.is_empty() {
        return Ok("(no pull requests)".into());
    }
    Ok(out.trim_end().to_string())
}

fn cmd_pr_comment(args: &[String]) -> Result<String, CliError> {
    const USAGE: &str = "usage: git forge pr comment <n> <body>";
    // Same strict flag/token split as its issue sibling: an unknown option
    // is a clean usage error naming the flag (VAL-202), never a
    // silently-ignored prefix of the comment body.
    let (json, args) = machine::json_requested(args);
    machine::reject_flags(&args, USAGE)?;
    if args.len() < 2 {
        return Err(CliError::usage(USAGE, ""));
    }
    let id = parse_entity_id(&args[0])?;
    let body = args[1..].join(" ").trim().to_string();
    if body.is_empty() {
        return Err(CliError::usage("comment body must be non-empty", ""));
    }
    let (store, actor) = open_mutation_store()?;
    let r = crate::store::pr_head_ref(id);
    if !store_has_ref(store.store(), &r) {
        return Err(CliError::not_found(
            format!("PR #{id} does not exist"),
            id.to_string(),
        ));
    }
    let ev = Event::new(
        EventKind::PrComment,
        "pr",
        id,
        &actor,
        body_obj(&[("body", json_str(&body))]),
    );
    let event = store.append_event(&r, &ev).map_err(CliError::from)?;
    if json {
        let state = machine::pr_state_at(store.store(), event)?;
        return Ok(machine::event_result_json(id, state, &event.to_string()));
    }
    Ok(format!("comment added to PR #{id}"))
}

/// `git forge pr abandon <n> --reason <text>` — abandon is terminal: appends a
/// single `pr.abandoned` event (body `{"reason": <text>}`) to the PR `/head`
/// chain. Refuses an already-merged PR, a second abandon, and an unknown PR;
/// every refusal writes nothing. The reason is required and must survive
/// trimming (whitespace-only is rejected before any write). No reopen: a
/// reopened PR is a new entity.
fn cmd_pr_abandon(args: &[String]) -> Result<String, CliError> {
    const USAGE: &str = "usage: git forge pr abandon <n> --reason <text>";
    let (json, args) = machine::json_requested(args);
    let id = parse_entity_id(args.first().ok_or_else(|| CliError::usage(USAGE, ""))?)?;
    let mut reason: Option<String> = None;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--reason" => {
                i += 1;
                if i >= args.len() {
                    return Err(CliError::usage("--reason requires a text", ""));
                }
                reason = Some(args[i].clone());
            }
            h @ ("-h" | "--help") => return help_result(json, h, USAGE.into()),
            a if a.starts_with('-') => {
                return Err(CliError::usage(format!("unknown option '{a}'"), a))
            }
            other => {
                return Err(CliError::usage(
                    format!(
                    "unexpected argument '{other}'; usage: git forge pr abandon <n> --reason <text>"
                ),
                    "",
                ))
            }
        }
        i += 1;
    }
    let reason = reason.ok_or_else(|| CliError::usage(USAGE, ""))?;
    if reason.trim().is_empty() {
        return Err(CliError::usage(
            "--reason is required and must be non-empty",
            "",
        ));
    }
    // Read-only path first: PR existence and terminality are validated WITHOUT
    // resolving the mutable identity, so every refusal resolves nothing and
    // writes nothing. The append below reopens the store bound to the actor.
    let store = open_store()?;
    let r = crate::store::pr_head_ref(id);
    if !store_has_ref(&store, &r) {
        return Err(CliError::not_found(
            format!("PR #{id} does not exist"),
            id.to_string(),
        ));
    }
    let chain = store.read_chain(&r).map_err(CliError::from)?;
    let state = crate::event::fold(&chain).pr;
    if state.merge_result.is_some() {
        return Err(CliError::invalid(format!("PR #{id} is already merged")));
    }
    if let Some(original) = &state.abandoned {
        return Err(CliError::invalid(format!(
            "PR #{id} is already abandoned: {original}"
        )));
    }
    let (store, actor) = open_mutation_store()?;
    let ev = Event::new(
        EventKind::PrAbandoned,
        "pr",
        id,
        &actor,
        body_obj(&[("reason", json_str(&reason))]),
    );
    let event = store.append_event(&r, &ev).map_err(CliError::from)?;
    if json {
        let state = machine::pr_state_at(store.store(), event)?;
        return Ok(machine::pr_abandon_json(
            id,
            state,
            &reason,
            &event.to_string(),
        ));
    }
    Ok(format!("PR #{id} abandoned: {reason}"))
}

/// Dispatch a `git forge pr` subcommand. `argv` excludes the `pr` token.
pub fn run_pr(argv: &[String]) -> Result<String, CliError> {
    let sub = argv.first().map(|s| s.as_str()).unwrap_or("");
    let (json, rest) = machine::json_requested(argv.get(1..).unwrap_or(&[]));
    if help_requested(&rest) {
        let usage = match sub {
            "merge" => Some(crate::pr_merge::pr_merge_help()),
            "prepare" => Some(crate::pr_prepare::pr_prepare_help()),
            _ => pr_sub_usage(sub).map(String::from),
        };
        if let Some(usage) = usage {
            return help_result(json, &rest[0], usage);
        }
    }
    match sub {
        "create" => crate::pr_create::cmd_pr_create(&argv[1..]),
        "list" => cmd_pr_list(&argv[1..]),
        "show" => {
            if argv[1..].iter().all(|arg| arg == "--json") {
                return Err(CliError::usage("usage: git forge pr show <n> [--json]", ""));
            }
            cmd_pr_show(&argv[1..])
        }
        "comment" => cmd_pr_comment(&argv[1..]),
        "review" => crate::pr_review::cmd_pr_review(&argv[1..]),
        "update" => crate::pr_merge::cmd_pr_update(&argv[1..]),
        "prepare" => crate::pr_prepare::cmd_pr_prepare(&argv[1..]),
        "diff" => {
            if argv[1..].iter().all(|arg| arg == "--json") {
                return Err(CliError::usage("usage: git forge pr diff <n> [--json]", ""));
            }
            crate::pr_merge::cmd_pr_diff(&argv[1..])
        }
        "merge" => crate::pr_merge::cmd_pr_merge(&argv[1..]),
        "abandon" => cmd_pr_abandon(&argv[1..]),
        "help" | "-h" | "--help" => help_result(json, sub, pr_help()),
        "" => Ok(pr_help()),
        other => Err(CliError::usage(
            format!("unknown pr subcommand '{other}'"),
            other,
        )),
    }
}

fn pr_sub_usage(sub: &str) -> Option<&'static str> {
    Some(match sub {
        "create" => "usage: git forge pr create --source <branch> --base <branch> <title> [--body <text>] [--label <x>]... [--closes <n>]",
        "list" => "usage: git forge pr list",
        "show" => "usage: git forge pr show <n>",
        "comment" => "usage: git forge pr comment <n> <body> [--json]",
        "review" => "usage: git forge pr review <n> --approve|--reject [--file <f> --line <l> --commit <c>] [--json]",
        "abandon" => "usage: git forge pr abandon <n> --reason <text> [--json]",
        "diff" => "usage: git forge pr diff <n> [--json]",
        "update" => "usage: git forge pr update <n> [--json]",
        "prepare" => "usage: git forge pr prepare <n> --strategy merge|squash|rebase|ff [--json]",
        _ => return None,
    })
}

fn pr_help() -> String {
    "usage: git forge pr <create|list|show|comment|review|update|prepare|diff|merge|abandon> ...\n\
     \nsubcommands:\n\
     \x20 create --source <branch> --base <branch> <title> [--body <text>] [--label <x>]... [--closes <n>]\n\
     \x20 list\n\
     \x20 show <n> [--json]\n\
     \x20 comment <n> <body> [--json]\n\
     \x20 review <n> --approve|--reject [--file <f> --line <l> --commit <c>] [--json]\n\
     \x20 update <n> [--json]\n\
     \x20 prepare <n> --strategy merge|squash|rebase|ff [--json]\n\
     \x20 diff <n> [--json]\n\
     \x20 merge [<n>] [--merge|--squash|--rebase|--ff] [--json]\n\
     \x20 abandon <n> --reason <text> [--json]"
        .to_string()
}

// ─────────────────────────── CI commands (t0) ───────────────────────────

/// Detect a leftover disposable worktree after a removal attempt (F-002/F-003):
/// best-effort remove `tmp`, then report WHY it is not fully cleaned (removal
/// failed, directory still present, verification failed, or still registered) —
/// or None when it is gone. Shared by `cmd_ci_run`'s happy-path and add-failure
/// cleanup so a leftover is always reported, never silently discarded.
fn worktree_leftover(repo_dir: &std::path::Path, tmp: &std::path::Path) -> Option<String> {
    crate::git::worktree_remove_ci(repo_dir, tmp)
        .err()
        .map(|e| format!("temp worktree removal failed: {e}"))
        .or_else(|| {
            let (ok, list, err_l) = crate::git::worktree_list_raw_ci(repo_dir);
            if tmp.exists() {
                Some("temp worktree directory still exists".to_string())
            } else if !ok {
                Some(format!("worktree verification failed: {err_l}"))
            } else if crate::git::worktree_registered_path(&list, tmp) {
                Some("temp worktree still registered".to_string())
            } else {
                None
            }
        })
        .map(|reason| format!("{reason} (worktree left at {})", tmp.display()))
}

/// `git forge ci run <pr>` — execute the repo CI plan against the PR's own
/// commits in a temporary worktree, append a CI Check event recording the
/// outcome, and leave the developer's working tree and current branch
/// untouched. The plan is `.forge/ci.sh` when present in the PR's immutable
/// source snapshot as a regular file, otherwise the `just check` fallback; a
/// `.forge/ci.sh` that is a symlink or otherwise not a regular file is refused
/// up front (F-001), so the plan always comes from immutable PR content.
/// The `ci run` READ-ONLY refusal path: PR existence, anchored chain (F-008)
/// and the abandoned-terminal check, before any identity resolution or
/// worktree is created — every refusal resolves nothing and writes nothing.
/// Returns the head ref (for the later append) and the folded state. Split
/// from `cmd_ci_run` so that function stays under the too_many_lines cap.
fn ci_run_read_only_check(
    store: &EventStore,
    id: u64,
) -> Result<(String, crate::event::PrState), CliError> {
    let head = crate::store::pr_head_ref(id);
    if !store_has_ref(store, &head) {
        return Err(CliError::not_found(
            format!("PR #{id} does not exist"),
            id.to_string(),
        ));
    }
    // Anchored read (F-008): the WHOLE chain folded here must be anchored to
    // PR #id's authoritative pr.created — a forged/foreign chain must not
    // supply the snapshot fields the run executes against.
    let head_tip = store
        .repo()
        .find_reference(&head)
        .and_then(|r| r.target().ok_or(git2::Error::from_str("no target")))
        .map_err(|_| format!("PR #{id} does not exist"))?;
    let (anchored, chain) = store
        .read_pr_chain_anchored(id, head_tip)
        .map_err(CliError::from)?;
    if !anchored {
        return Err((format!(
            "refs/forge/prs/{id}/head points at a tip whose chain is not anchored \
             to PR #{id}"
        ))
        .into());
    }
    let state = crate::event::fold(&chain).pr;
    // Abandoned PRs are terminal (pr.abandoned): refuse BEFORE the plan is
    // materialized or any worktree is created — no ci.check is appended and
    // the plan never executes, so the PR's existing CI state stays as is.
    if let Some(reason) = &state.abandoned {
        return Err(CliError::invalid(format!(
            "PR #{id} is abandoned: {reason}"
        )));
    }
    Ok((head, state))
}
fn cmd_ci_run(args: &[String]) -> Result<String, CliError> {
    // `--json` (#7): the recorded Check's outcome as one JSON line on stdout
    // (success path only; failures keep the prose stderr + exit 1 contract).
    let (json, args) = machine::json_requested(args);
    if args.len() != 1 {
        // Name the offending token so a rejected flag is diagnosable
        // (unknown flag keeps exit-1, VAL-202).
        return Err(CliError::usage(
            match args.first() {
                Some(a) if a.starts_with('-') => {
                    format!("unknown option '{a}'; usage: git forge ci run <pr>")
                }
                _ => format!(
                    "usage: git forge ci run <pr> (got {} argument(s), expected exactly 1)",
                    args.len()
                ),
            },
            args.first().map(String::as_str).unwrap_or(""),
        ));
    }
    let id = parse_entity_id(args.first().map(|s| s.as_str()).unwrap_or(""))?;
    // Read-only refusal path + identity binding live in the extracted helper
    // below (keeps this function under the too_many_lines cap).
    let store = open_store()?;
    let (head, state) = ci_run_read_only_check(&store, id)?;
    let (store, actor) = open_mutation_store()?;
    // The plan runs against the PR's CURRENT generation's immutable source
    // snapshot (its own commits), never the developer's working tree.
    let generation = state.generation;
    let source_oid = store
        .repo()
        .find_reference(&crate::store::pr_source_ref_for(id, generation))
        .and_then(|r| r.target().ok_or(git2::Error::from_str("no target")))
        .map_err(|_| format!("PR #{id} snapshot source ref missing"))?;
    let base_oid = store
        .repo()
        .find_reference(&crate::store::pr_base_ref_for(id, generation))
        .and_then(|r| r.target().ok_or(git2::Error::from_str("no target")))
        .map_err(|_| format!("PR #{id} snapshot base ref missing"))?;
    // F-008: the snapshot refs must still point at the AUTHORITATIVE
    // snapshot of the current generation (shared invariant, one owner in
    // the store).
    crate::store::EventStore::validate_snapshot_against_state(id, &state, source_oid, base_oid)?;
    // F-002 candidate-bound CI: when the PR has a VALID prepared candidate
    // (fold candidate whose generation matches the current snapshot AND
    // whose identity is corroborated by the pinned `refs/forge/prs/<n>/result`
    // ref), the Check validates the CANDIDATE RESULT, not the source
    // snapshot. The candidate identity is CAPTURED HERE at run start and
    // stamped into the check body after execution — a concurrent
    // re-prepare/update during the run cannot swap the identity the check
    // actually validated (Main directive: captured identity, never
    // fresh-fold values). A stale/mismatched candidate falls back to the
    // legacy source-snapshot run (unbound check; the candidate merge gate
    // refuses until a fresh candidate-bound run).
    let candidate_run = match &state.prepared {
        Some(p) if p.generation == generation => store
            .repo()
            .find_reference(&crate::store::pr_result_ref(id))
            .ok()
            .and_then(|r| r.target())
            .filter(|pin| pin.to_string() == p.result_commit)
            .map(|_| p.clone()),
        _ => None,
    };
    let run_oid = match &candidate_run {
        Some(c) => c
            .result_commit
            .parse::<git2::Oid>()
            .map_err(|_| "PR prepared candidate result_commit invalid".to_string())?,
        None => source_oid,
    };
    // Live base tip captured at run start (#2 result-bound CI): the Check
    // binds the tip it RAN against, so a concurrent base move during the
    // run cannot re-bind it.
    let live_base =
        crate::pr_prepare::live_base_head_entry(store.repo(), state.base_ref.as_deref());
    let repo_dir = store
        .repo()
        .workdir()
        .map(|p| p.to_path_buf())
        .ok_or_else(|| "cannot resolve repository working directory".to_string())?;

    // Determine the plan from the RUN OID's IMMUTABLE bytes up front
    // (F-001/F-012): a symlink/non-regular `.forge/ci.sh`, or a symlink
    // justfile for the `just check` fallback, is refused before a worktree is
    // even created, so CI never follows a tracked link to mutable bytes
    // outside the detached worktree. Absent `.forge/ci.sh` -> the pinned
    // `just check` fallback (F-012). For a candidate run the plan comes from
    // the RESULT commit's tree.
    let plan = crate::ci::snapshot_ci_plan(store.repo(), run_oid)?;

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
    // Release the path lock once the worktree is registered (or once a reserved
    // path is abandoned); every early return after the loop must drop it too.
    let release_lock = |lock_handle: &mut Option<std::fs::File>, path: &std::path::Path| {
        *lock_handle = None;
        let _ = std::fs::remove_file(path.with_extension("lock"));
    };
    let (ok_wt, err_wt) = loop {
        let lock = tmp.with_extension("lock");
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock)
        {
            Ok(h) => {
                lock_handle = Some(h);
                match crate::git::worktree_add_ci(&repo_dir, &tmp, &run_oid) {
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
        // A failed `git worktree add` can leave a partially-created/registered
        // worktree behind. Remove it and report a leftover the same way as the
        // happy path (never silently discarded); an Err from removing a path the
        // add never registered (clean early-validation failure) is no leftover.
        let leftover = worktree_leftover(&repo_dir, &tmp);
        release_lock(&mut lock_handle, &tmp);
        return Err((match leftover {
            Some(l) => format!("failed to create temporary worktree for CI run: {err_wt}; {l}"),
            None => format!("failed to create temporary worktree for CI run: {err_wt}"),
        })
        .into());
    }

    // Run the plan. The exact immutable snapshot bytes are materialized and
    // executed (F-013), the wait is bounded by `GIT_FORGE_CI_TIMEOUT`
    // (clamped, F-011), and on expiry the WHOLE process group is killed and
    // reaped (F-015) so a background descendant cannot survive the deadline.
    let timeout = crate::ci::ci_timeout(std::env::var("GIT_FORGE_CI_TIMEOUT").ok().as_deref());
    let run = crate::ci::run_ci_plan(store.repo(), run_oid, &tmp, &plan, timeout);

    // Clean up the temp worktree regardless of the plan outcome, holding the
    // sibling lock until the worktree is removed AND verified gone (directory
    // absent + path unregistered), mirroring the merge-path postconditions
    // (F-002/F-003): a leftover is reported, never silently discarded.
    let leftover = worktree_leftover(&repo_dir, &tmp);
    release_lock(&mut lock_handle, &tmp);

    // F-025: do NOT publish a green Check until cleanup verified succeeded — a
    // passing plan that locks its worktree (so removal fails) must leave a
    // failed/non-green latest Check. A run that exceeded its bounded deadline
    // (`timed_out`) is ALSO never green, even if the child happened to exit 0
    // right at the deadline (the group may have been killed / a descendant
    // escaped), so the merge gate cannot accept it.
    let status = if run.status == Some(0) && !run.timed_out && leftover.is_none() {
        "success"
    } else {
        "failed"
    };
    let recorded = crate::pr_prepare::record_and_report_ci_check(
        &store,
        &head,
        id,
        status,
        plan.label(),
        generation,
        live_base,
        candidate_run.as_ref(),
        leftover,
        run.status,
        run.timed_out,
        run.detail.map(|d| d.to_string()),
        &actor,
    );
    if json {
        // The Check was recorded (record_and_report returns Ok only for a
        // verified-green run); failures fall through to the prose stderr +
        // exit-1 contract, so stdout JSON always describes a recorded Check.
        return recorded.map(|_| machine::ci_run_json(id, status, plan.label(), generation));
    }
    recorded
}

/// Dispatch a `git forge ci` subcommand. `argv` excludes the `ci` token.
pub fn run_ci(argv: &[String]) -> Result<String, CliError> {
    let sub = argv.first().map(|s| s.as_str()).unwrap_or("");
    let (json, rest) = machine::json_requested(argv.get(1..).unwrap_or(&[]));
    if help_requested(&rest) && sub == "run" {
        return help_result(json, &rest[0], "usage: git forge ci run <pr>".into());
    }
    match sub {
        "run" => cmd_ci_run(&argv[1..]),
        "help" | "-h" | "--help" => help_result(json, sub, ci_help()),
        "" => Ok(ci_help()),
        other => Err(CliError::usage(
            format!("unknown ci subcommand '{other}'"),
            other,
        )),
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
