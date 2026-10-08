//! `git forge pr prepare <n> --strategy <s>` (F-002): materialize the PR's
//! merge candidate against CURRENT state and pin it immutably.
//!
//! The prepared candidate is the single artifact the whole result-bound
//! pipeline validates: `ci run` executes the plan against the candidate
//! result commit, `pr review --approve` binds the decision to the candidate,
//! and `pr merge` CASes the base branch FROM the candidate's base_head TO
//! its result_commit — never re-materializing at merge time. This module
//! owns the prepare command; the candidate gate lives in `merge_state`.

use crate::cli_error::CliError;

use std::collections::HashMap;

use git2::Oid;

use crate::event::{Event, EventKind, JsonValue};
use crate::store::{pr_head_ref, pr_result_ref, EventStore, StoreError, ZERO_OID};

fn prepare_arguments(args: &[String]) -> Result<(u64, String), CliError> {
    let mut id_arg: Option<String> = None;
    let mut strategy: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--strategy" => {
                i += 1;
                if i >= args.len() {
                    return Err(CliError::usage("--strategy requires a value", ""));
                }
                if strategy.is_some() {
                    return Err(CliError::usage("--strategy specified more than once", ""));
                }
                strategy = Some(args[i].clone());
            }
            // Reached only under `--json`: without it any help token already
            // printed help before parsing.
            h @ ("-h" | "--help") => return Err(crate::cli::json_help_refused(h)),
            s if s.starts_with('-') => {
                return Err(CliError::usage(format!("unknown option '{s}'"), s))
            }
            s => {
                if id_arg.is_some() {
                    return Err(CliError::usage("usage: git forge pr prepare <n> --strategy <merge|squash|rebase|ff> (only one PR id allowed)", s));
                }
                id_arg = Some(s.to_string());
            }
        }
        i += 1;
    }
    let strategy = strategy.ok_or_else(|| {
        CliError::usage(
            "usage: git forge pr prepare <n> --strategy <merge|squash|rebase|ff> \
         (--strategy is required)",
            "",
        )
    })?;
    if !matches!(strategy.as_str(), "merge" | "squash" | "rebase" | "ff") {
        return Err(CliError::usage(
            format!("unknown strategy '{strategy}'; use --strategy merge|squash|rebase|ff"),
            &strategy,
        ));
    }
    let id = crate::cli::parse_entity_id(id_arg.as_deref().ok_or_else(|| {
        CliError::usage(
            "usage: git forge pr prepare <n> --strategy <merge|squash|rebase|ff>",
            "",
        )
    })?)?;
    Ok((id, strategy))
}

/// `git forge pr prepare <n> --strategy <merge|squash|rebase|ff>`
///
/// Materialize the strategy in a temporary worktree detached at the CURRENT
/// base tip (or the source snapshot for rebase), against the PR's current
/// snapshot generation. Appends `pr.prepared` (body: generation, source_head,
/// base_head = CURRENT base tip, strategy, result_commit, result_tree,
/// merge_base) and CAS-pins `refs/forge/prs/<n>/result` → result_commit in
/// ONE atomic ref batch (`BoundEventStore::prepare_pr`). Re-prepare REPLACES
/// the prior candidate (CAS old→new); it does NOT bump the generation. The
/// stdout line and `pr show`'s additive `prepared:` line are the adapter
/// contract surface.
pub(crate) fn cmd_pr_prepare(args: &[String]) -> Result<String, CliError> {
    let (json, args) = crate::machine::json_requested(args);
    // Without `--json` any help token prints help (existing behavior). Under
    // `--json` the parser below decides: a standalone help token is refused,
    // and a `--strategy` value is validated as a strategy.
    if !json
        && args
            .iter()
            .any(|arg| matches!(arg.as_str(), "-h" | "--help"))
    {
        return Ok(pr_prepare_help());
    }
    let (id, strategy) = prepare_arguments(&args)?;
    // Read-only path first: existence and terminality resolve nothing and
    // write nothing.
    let store = open_read_store()?;
    let head = crate::store::pr_head_ref(id);
    if !crate::cli::store_has_ref(&store, &head) {
        return Err(CliError::not_found(
            format!("PR #{id} does not exist"),
            id.to_string(),
        ));
    }
    // Anchored read (F-008): only PR #id's own chain supplies the snapshot.
    let head_tip = store
        .repo()
        .find_reference(&head)
        .and_then(|r| r.target().ok_or(git2::Error::from_str("no target")))
        .map_err(|_| CliError::not_found(format!("PR #{id} does not exist"), id.to_string()))?;
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
    if let Some(reason) = &state.abandoned {
        return Err(CliError::invalid(format!(
            "PR #{id} is abandoned: {reason}"
        )));
    }
    if state.merge_result.is_some() {
        return Err(CliError::invalid(format!("PR #{id} is already merged")));
    }
    let base_ref = state
        .base_ref
        .clone()
        .ok_or_else(|| "PR has no base_ref".to_string())?;
    let generation = state.generation;
    let source_oid = store
        .repo()
        .find_reference(&crate::store::pr_source_ref_for(id, generation))
        .and_then(|r| r.target().ok_or(git2::Error::from_str("no target")))
        .map_err(|_| format!("PR #{id} snapshot source ref missing"))?;
    let snapshot_base = store
        .repo()
        .find_reference(&crate::store::pr_base_ref_for(id, generation))
        .and_then(|r| r.target().ok_or(git2::Error::from_str("no target")))
        .map_err(|_| format!("PR #{id} snapshot base ref missing"))?;
    // Shared snapshot invariant (F-008): the snapshot refs must still point
    // at the authoritative snapshot in the fold.
    crate::store::EventStore::validate_snapshot_against_state(
        id,
        &state,
        source_oid,
        snapshot_base,
    )?;
    // The candidate is built against the CURRENT base-branch tip (live-base
    // refresh, #2): a base move since the snapshot is what make prepare the
    // right tool — the candidate visibly captures the moved-base result.
    let current_base = store
        .repo()
        .find_reference(&format!("refs/heads/{base_ref}"))
        .and_then(|r| {
            r.target()
                .ok_or(git2::Error::from_str("base branch has no target"))
        })
        .map_err(|_| format!("base branch '{base_ref}' does not exist"))?;
    // Single merge base between the snapshot source and the CURRENT base tip
    // (the strategy execution needs it; a criss-cross refuses).
    let exec_merge_base =
        crate::git::require_single_merge_base(store.repo(), source_oid, current_base)?;
    // --ff precondition (VAL-102, shared with merge): the source must sit
    // directly on top of the CURRENT base tip. Any other strategy is fine
    // (merge/squash/rebase), matching the merge command's own precondition.
    if strategy == "ff" && exec_merge_base != current_base {
        return Err((format!(
            "PR #{id} cannot fast-forward: the source does not sit directly on \
             top of the current base tip of '{base_ref}' (merge_base \
             {exec_merge_base} != base tip {current_base}); use --strategy \
             rebase to replay the source or recreate the PR"
        ))
        .into());
    }
    // The PR title is required by squash only (VAL-101); validate up front so
    // the refusal happens before any worktree side effect.
    if strategy == "squash" && state.title.as_deref().unwrap_or("").trim().is_empty() {
        return Err("PR has no title; cannot prepare a squash candidate".into());
    }
    let repo_dir = store
        .repo()
        .workdir()
        .map(|p| p.to_path_buf())
        .ok_or_else(|| "cannot resolve repository working directory".to_string())?;
    // Resolve the actor NOW (before any side effect) and bind the mutation
    // store: every forge write below uses the pre-bound signature.
    let (signature, actor) = crate::identity::resolve_identity()?;
    let (store, actor) = crate::identity::bind_identity(store, signature, actor);

    // Materialize the candidate: temp worktree + strategy execution + tree
    // resolution + worktree cleanup (helper keeps cmd_pr_prepare under the
    // 150-line clippy gate).
    let (result_commit, result_tree) = materialize_candidate(MaterializeInputs {
        store: &store,
        repo_dir: &repo_dir,
        id,
        strategy: &strategy,
        title: state.title.as_deref().unwrap_or(""),
        base_ref: &base_ref,
        source_ref: state.source_ref.as_deref().unwrap_or(""),
        source_oid,
        current_base,
        exec_merge_base,
    })?;
    // Publish atomically: pr.prepared event + result pin in ONE update-ref
    // batch (all-or-nothing; a CAS conflict re-parents + retries).
    let event = prepare_pr(
        &store,
        id,
        generation,
        source_oid,
        current_base,
        exec_merge_base,
        &strategy,
        result_commit,
        result_tree,
        &actor,
    )
    .map_err(CliError::from)?;
    if json {
        return Ok(crate::machine::pr_prepare_json(
            id,
            crate::machine::pr_state_at(store.store(), event)?,
            &strategy,
            generation,
            &result_commit.to_string(),
            &result_tree.to_string(),
            &current_base.to_string(),
            &event.to_string(),
        ));
    }
    Ok(format!(
        "prepared PR #{id} for strategy {strategy} (candidate {result_commit}, \
         generation {generation}, base {current_base})"
    ))
}

/// Open the read store (thin wrapper for symmetry with cmd_pr_update's
/// read-then-reopen pattern; the mutation store is bound inside).
fn open_read_store() -> Result<EventStore, CliError> {
    crate::cli::open_store()
}

/// Inputs of one candidate materialization (grouped so the helper stays
/// under the clippy too_many_arguments gate).
struct MaterializeInputs<'a> {
    store: &'a crate::store::BoundEventStore,
    repo_dir: &'a std::path::Path,
    id: u64,
    strategy: &'a str,
    title: &'a str,
    base_ref: &'a str,
    source_ref: &'a str,
    source_oid: git2::Oid,
    current_base: git2::Oid,
    exec_merge_base: git2::Oid,
}

/// Materialize the candidate in a temp worktree (detached at the CURRENT
/// base tip, or the source snapshot for rebase); returns (result_commit,
/// result_tree) and cleans up the worktree. Errors carry the user-facing
/// message with the worktree already removed best-effort.
fn materialize_candidate(
    inputs: MaterializeInputs<'_>,
) -> Result<(git2::Oid, git2::Oid), CliError> {
    let MaterializeInputs {
        store,
        repo_dir,
        id,
        strategy,
        title,
        base_ref,
        source_ref,
        source_oid,
        current_base,
        exec_merge_base,
    } = inputs;
    // Temp worktree path (same collision-avoidance scheme as merge/ci:
    // hash the canonical workdir + pid + retry suffix; sibling lock file).
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    repo_dir.to_string_lossy().as_bytes().hash(&mut hasher);
    let nonce = hasher.finish();
    let mut attempt = 0u32;
    let mk = |attempt: u32| {
        std::env::temp_dir().join(format!(
            "git-forge-pr{id}-prepare-{nonce:x}-{}-{attempt}",
            std::process::id()
        ))
    };
    let mut tmp = mk(0);
    let mut lock_handle: Option<std::fs::File> = None;
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
                // Rebase starts detached at /source; everything else
                // detached at the CURRENT base tip (#2 live-base).
                let detach_oid = if strategy == "rebase" {
                    source_oid
                } else {
                    current_base
                };
                match crate::git::worktree_add(repo_dir, &tmp, &detach_oid) {
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
        release_lock(&mut lock_handle, &tmp);
        return Err(format!("failed to create temporary worktree: {err_wt}").into());
    }
    // Execute the strategy in the worktree; result_commit = worktree HEAD.
    let exec = crate::git::execute_strategy(
        repo_dir,
        &tmp,
        strategy,
        source_oid,
        current_base,
        exec_merge_base,
        title,
    );
    let result_commit = match exec {
        Ok(o) => o,
        Err(e) => {
            release_lock(&mut lock_handle, &tmp);
            return Err(if e.conflict_paths.is_empty() {
                CliError::from(e.message)
            } else {
                prepare_conflict_error(&PrepareConflict {
                    id,
                    strategy,
                    base_ref,
                    source_ref,
                    base: current_base,
                    source: source_oid,
                    paths: &e.conflict_paths,
                    cleanup: &e.cleanup,
                })
            });
        }
    };
    let result_commit = result_commit.parse::<git2::Oid>().map_err(|_| {
        crate::git::cleanup_failed_worktree(
            repo_dir,
            &tmp,
            "prepare",
            "invalid result commit".into(),
        )
    })?;
    // The result commit's tree: the identity `ci run` validates and the
    // merge lands.
    let result_tree = store
        .repo()
        .find_commit(result_commit)
        .and_then(|c| c.tree())
        .map(|t| t.id())
        .map_err(|_| {
            crate::git::cleanup_failed_worktree(
                repo_dir,
                &tmp,
                "prepare",
                "cannot read the candidate result tree".into(),
            )
        })?;
    // Remove the temp worktree BEFORE pinning the candidate: if removal
    // fails, nothing was published (no event, no ref) and the failure is
    // reported cleanly.
    if let Err(err_rm) = crate::git::worktree_remove(repo_dir, &tmp) {
        let _ = std::fs::remove_file(tmp.with_extension("lock"));
        return Err(format!(
            "strategy executed but temp worktree removal failed: {err_rm}                  (worktree left at {})",
            tmp.display()
        )
        .into());
    }
    if tmp.exists() {
        let _ = std::fs::remove_file(tmp.with_extension("lock"));
        return Err(format!(
            "strategy executed but temp worktree directory still exists at {}",
            tmp.display()
        )
        .into());
    }
    release_lock(&mut lock_handle, &tmp);
    Ok((result_commit, result_tree))
}

/// Most conflicted paths the human message lists; JSON `data.paths` is complete.
const MESSAGE_PATH_LIMIT: usize = 20;

/// One strategy execution that left unmerged index entries.
struct PrepareConflict<'a> {
    id: u64,
    strategy: &'a str,
    base_ref: &'a str,
    source_ref: &'a str,
    base: Oid,
    source: Oid,
    paths: &'a [String],
    cleanup: &'a str,
}

/// The `E_PREPARE_CONFLICT` failure: the message names the candidate inputs,
/// up to [`MESSAGE_PATH_LIMIT`] conflicted paths, the remedy, and the
/// cleanup outcome; `data` carries `pr`, `strategy`, `base`, `source`, and
/// every conflicted path.
fn prepare_conflict_error(conflict: &PrepareConflict<'_>) -> CliError {
    let PrepareConflict {
        id,
        strategy,
        base_ref,
        source_ref,
        base,
        source,
        paths,
        cleanup,
    } = *conflict;
    // Paths and branch names come from repository content; escape control
    // characters in the human message. Git quoting leaves C1 controls
    // (U+0080-U+009F) in paths unescaped; sanitizing catches them.
    let base_ref = crate::cli::sanitize_terminal(base_ref);
    let source_ref = crate::cli::sanitize_terminal(source_ref);
    let count = match paths.len() {
        1 => "1 path".to_string(),
        n => format!("{n} paths"),
    };
    let mut shown = paths
        .iter()
        .take(MESSAGE_PATH_LIMIT)
        .map(|path| crate::cli::sanitize_terminal(path))
        .collect::<Vec<_>>()
        .join(", ");
    if paths.len() > MESSAGE_PATH_LIMIT {
        shown.push_str(&format!(" (and {} more)", paths.len() - MESSAGE_PATH_LIMIT));
    }
    let mut error = CliError::new(
        "E_PREPARE_CONFLICT",
        format!(
            "PR #{id} cannot be prepared with strategy {strategy}: source {source} conflicts \
             with base '{base_ref}' at {base} in {count}: {shown}; merge '{base_ref}' into the \
             source branch '{source_ref}', resolve the conflicts, then run `git forge pr update \
             {id}` or create a replacement PR; {cleanup}"
        ),
        id.to_string(),
        format!(
            "Merge the base branch into the source branch, resolve the conflicts, then run \
             `git forge pr update {id}` or create a replacement PR."
        ),
        false,
    );
    error.data = vec![
        ("pr", JsonValue::Number(id as i64)),
        ("strategy", JsonValue::String(strategy.to_string())),
        ("base", JsonValue::String(base.to_string())),
        ("source", JsonValue::String(source.to_string())),
        ("paths", crate::event::json_string_array(paths)),
    ];
    error
}

/// ── F-002 prepared-candidate gates ── The merge result is the
/// PREPARED candidate (immutable, pinned at `refs/forge/prs/<n>/result` by
/// `pr prepare`), not a strategy re-executed at merge time. Three checks:
/// (1) the candidate is CURRENT — prepared from the PR's current snapshot
/// generation, built against the CURRENT base-branch tip (so a base move or
/// a `pr update` after prepare visibly invalidates it, refusing with a
/// re-prepare remedy instead of silently re-deriving the result); (2) the
/// candidate's strategy matches the merge's `--strategy` (a submitted
/// candidate is strategy-specific, so a strategy switch requires a fresh
/// prepare); (3) every gate input (approve + latest CI Check) is BOUND to
/// this exact candidate (strategy + result_commit + result_tree recorded at
/// review/CI time), so no path approves an unchecked or differently-checked
/// result. Each refusal precedes every side effect and names its own
/// remedy.
pub(crate) fn validate_prepared_candidate(
    id: u64,
    state: &crate::event::PrState,
    candidate: &crate::event::PreparedCandidate,
    base_ref: &str,
    current_base: Oid,
    strategy: &str,
) -> Result<(), CliError> {
    let gate_error = |message: String| {
        CliError::new(
            "E_GATE",
            message,
            id.to_string(),
            "Inspect PR review and CI state and satisfy the reported merge gate.",
            false,
        )
    };
    if candidate.generation != state.generation {
        return Err(gate_error(format!(
            "PR #{id} prepared candidate is stale (prepared on generation {}, current \
             snapshot generation {}); re-run `git forge pr prepare {id} --strategy \
             {strategy}`, then `git forge ci run {id}` and `git forge pr review {id} \
             --approve`, then merge",
            candidate.generation, state.generation
        )));
    }
    if candidate.base_head != current_base.to_string() {
        return Err(gate_error(format!(
            "PR #{id} prepared candidate is stale (built against base tip {}, current \
             base tip of '{base_ref}' is {current_base}); re-run `git forge pr prepare \
             {id} --strategy {strategy}`, then `git forge ci run {id}` and `git forge pr \
             review {id} --approve`, then merge",
            candidate.base_head
        )));
    }
    if candidate.strategy != strategy {
        return Err(gate_error(format!(
            "PR #{id} prepared candidate is for strategy '{candidate_strategy}' but the \
             merge requested '{strategy}'; re-run `git forge pr prepare {id} --strategy \
             {strategy}`, then `git forge ci run {id}` and `git forge pr review {id} \
             --approve`, then merge",
            candidate_strategy = candidate.strategy
        )));
    }
    // Approve bound to the candidate (issue #2: a review decision must bind
    // the result being merged). A legacy approve (no candidate identity) or
    // one bound to a different candidate refuses.
    if state.decision_strategy.as_deref() != Some(candidate.strategy.as_str())
        || state.decision_result_commit.as_deref() != Some(candidate.result_commit.as_str())
        || state.decision_result_tree.as_deref() != Some(candidate.result_tree.as_str())
    {
        return Err(gate_error(format!(
            "PR #{id} approve is not bound to the prepared candidate (generation {}, \
             base {}, result {}); re-approve after re-running the candidate CI: \
             `git forge pr review {id} --approve`, then merge",
            candidate.generation, candidate.base_head, candidate.result_commit
        )));
    }
    // Latest CI Check success AND bound to this candidate.
    match state.ci_status.as_deref() {
        Some("success")
            if state.ci_strategy.as_deref() == Some(candidate.strategy.as_str())
                && state.ci_result_commit.as_deref() == Some(candidate.result_commit.as_str())
                && state.ci_result_tree.as_deref() == Some(candidate.result_tree.as_str()) => {}
        Some("success") => {
            return Err(gate_error(format!(
                "PR #{id} is not mergeable: latest CI Check is valid but not bound to \
                 the prepared candidate (generation {}, base {}, result {}); run \
                 `git forge ci run {id}` against the candidate, then merge",
                candidate.generation, candidate.base_head, candidate.result_commit
            )));
        }
        Some(status) => {
            return Err(gate_error(format!(
                "PR #{id} is not mergeable: latest CI Check is `{status}` (expected \
                 `success`); run `git forge ci run {id}`, then merge"
            )));
        }
        None => {
            return Err(gate_error(format!(
                "PR #{id} is not mergeable: no CI Check recorded; run `git forge ci run {id}`, then merge"
            )));
        }
    }
    Ok(())
}

/// Pin a materialized merge candidate for PR `id` (F-002 `pr prepare`):
/// append the `pr.prepared` event commit (sole parent of the observed
/// `/head` tip; body: generation, source_head, base_head, strategy,
/// result_commit, result_tree, merge_base) and CAS
/// `refs/forge/prs/<id>/result` → `result_commit`, all-or-nothing in ONE
/// ref transaction (mirrors `update_pr`). The result ref REUSES the
/// transient pending-result ref name: prepare creates/pins it (replace
/// allowed — re-prepare supersedes the prior candidate), merge
/// finalization CAS-deletes it.
///
/// Retry (bounded, like `update_pr`): on CAS conflict the SAME event
/// (retained UUID) is re-parented onto the fresh tip and the result ref
/// expected-old is re-read. Returns the event commit OID.
#[allow(clippy::too_many_arguments)]
pub(crate) fn prepare_pr(
    store: &crate::store::BoundEventStore,
    id: u64,
    generation: u64,
    source_head: Oid,
    base_head: Oid,
    merge_base: Oid,
    strategy: &str,
    result_commit: Oid,
    result_tree: Oid,
    actor: &str,
) -> Result<Oid, StoreError> {
    let head = pr_head_ref(id);
    let result = pr_result_ref(id);
    let mut body = HashMap::new();
    body.insert("generation".into(), JsonValue::Number(generation as i64));
    body.insert(
        "source_head".into(),
        JsonValue::String(source_head.to_string()),
    );
    body.insert("base_head".into(), JsonValue::String(base_head.to_string()));
    body.insert(
        "merge_base".into(),
        JsonValue::String(merge_base.to_string()),
    );
    body.insert("strategy".into(), JsonValue::String(strategy.to_string()));
    body.insert(
        "result_commit".into(),
        JsonValue::String(result_commit.to_string()),
    );
    body.insert(
        "result_tree".into(),
        JsonValue::String(result_tree.to_string()),
    );
    let event = Event::new(EventKind::PrPrepared, "pr", id, actor, body);
    let message = format!("forge:{}:{}", event.kind.as_str(), event.entity_id);
    let mut tip = store.store().current_tip(&head)?;
    for _ in 0..crate::store::MAX_APPEND_RETRIES {
        let parents: Vec<Oid> = tip.into_iter().collect();
        let event_oid = store.write_event_commit(&event, &parents, &message)?;
        let result_now = store.store().current_tip(&result)?;
        let result_old = result_now
            .map(|o| o.to_string())
            .unwrap_or_else(|| ZERO_OID.to_string());
        let head_old = tip
            .map(|o| o.to_string())
            .unwrap_or_else(|| ZERO_OID.to_string());
        let lines = vec![
            // Re-prepare replaces the prior candidate pin (CAS old→new).
            format!("update {result} {result_commit} {result_old}"),
            format!("update {head} {event_oid} {head_old}"),
        ];
        match store.store().run_update_ref_stdin(&lines) {
            Ok(()) => return Ok(event_oid),
            Err(_) => {
                let fresh = store.store().current_tip(&head)?;
                if fresh == tip {
                    // Head unchanged: the CAS failure is genuine (result
                    // ref moved concurrently or a git error) — refuse
                    // rather than retry into an unknown state.
                    return Err(StoreError::CasConflict);
                }
                tip = fresh;
            }
        }
    }
    Err(StoreError::Exhausted)
}

/// Build the `ci.check` body (status/plan/generation/base_head/candidate
/// identity — the F-001 captured generation and the F-002 candidate binding)
/// and append it to the PR head chain, then shape the stdout/err verdict.
/// Extracted from `cmd_ci_run` (clippy too_many_lines).
#[allow(clippy::too_many_arguments)]
pub(crate) fn record_and_report_ci_check(
    store: &crate::store::BoundEventStore,
    head: &str,
    id: u64,
    status: &str,
    plan_label: &str,
    generation: u64,
    live_base: Option<JsonValue>,
    candidate_run: Option<&crate::event::PreparedCandidate>,
    leftover: Option<String>,
    run_status: Option<i32>,
    ran_out: bool,
    run_detail: Option<String>,
    actor: &str,
) -> Result<String, CliError> {
    let mut body = HashMap::new();
    body.insert("status".into(), crate::cli::json_str(status));
    body.insert("plan".into(), crate::cli::json_str(plan_label));
    // Generation binding (F-001 regression): the Check validates the
    // snapshot generation it RAN against — captured at run start, never
    // re-read from a fresh fold after the run (an in-flight gen0 run whose
    // check appends after a `pr update` must record generation 0, so the
    // merge gate sees it as stale against gen 1).
    body.insert("generation".into(), JsonValue::Number(generation as i64));
    // #2 result-bound CI: the Check also records the LIVE base-branch tip at
    // run time, so the live-base merge gate can require a Check bound to the
    // current base tip when the base has moved since the PR snapshot.
    if let Some(bh) = live_base {
        body.insert("base_head".into(), bh);
    }
    // F-002 candidate-bound CI: when the run validated the prepared
    // candidate result, stamp the CAPTURED candidate identity (strategy,
    // result_commit, result_tree) — a concurrent re-prepare/update during
    // the run must not re-bind the check to a different candidate.
    if let Some(c) = candidate_run {
        body.insert("strategy".into(), crate::cli::json_str(&c.strategy));
        body.insert(
            "result_commit".into(),
            crate::cli::json_str(&c.result_commit),
        );
        body.insert("result_tree".into(), crate::cli::json_str(&c.result_tree));
    }
    let ev = Event::new(EventKind::CiCheck, "pr", id, actor, body);
    // Record the outcome — a failing plan still appends a `failure` CI Check
    // (VAL-002) before the command exits nonzero.
    let append = store.append_event(head, &ev);
    if let Some(ar) = append.as_ref().err() {
        let msg = format!("CI run completed but recording the CI Check failed: {ar}");
        let mut error = CliError::from(append.err().ok_or("CI append error missing")?);
        error.message = match leftover {
            Some(l) => format!("{msg}; temp worktree cleanup failed: {l}"),
            None => msg,
        };
        return Err(error);
    }

    if let Some(leftover) = leftover {
        return Err(CliError::new(
            "E_CI_FAILED",
            format!("CI run recorded {status} but temp worktree cleanup failed: {leftover}"),
            id.to_string(),
            "Inspect the reported worktree cleanup failure.",
            false,
        ));
    }
    append.map_err(|e| format!("CI run completed but recording the CI Check failed: {e}"))?;

    if run_status == Some(0) && !ran_out {
        Ok(format!("CI run for PR #{id} passed ({plan_label})"))
    } else {
        let detail = if ran_out {
            ": timed out".to_string()
        } else if let Some(d) = run_detail {
            format!(": {d}")
        } else {
            run_status
                .map(|c| format!(": exited with status {c}"))
                .unwrap_or_default()
        };
        Err(CliError::new(
            "E_CI_FAILED",
            format!("CI run for PR #{id} failed ({plan_label}){detail}"),
            id.to_string(),
            "Fix the CI plan and run CI again.",
            false,
        ))
    }
}

/// #2 result-bound gate input: the LIVE base-branch tip (`base_head` body
/// entry) for a `ci.check` or `pr.review` event, resolved at write time, so
/// the live-base merge gate can require gate inputs bound to the current
/// base tip when the base has moved since the PR snapshot. Extracted from
/// `cmd_ci_run` (clippy too_many_lines); `cmd_pr_review` calls it too.
pub(crate) fn live_base_head_entry(
    repo: &git2::Repository,
    base_ref: Option<&str>,
) -> Option<JsonValue> {
    repo.find_reference(&format!("refs/heads/{}", base_ref.unwrap_or("")))
        .ok()
        .and_then(|r| r.target())
        .map(|o| crate::cli::json_str(&o.to_string()))
}

/// `git forge pr prepare --help`
pub(crate) fn pr_prepare_help() -> String {
    "usage: git forge pr prepare <n> --strategy <merge|squash|rebase|ff> [--json]\n\
     \x20 Materialize the PR's merge candidate against the CURRENT base tip\n\
     \x20 and pin it (refs/forge/prs/<n>/result + pr.prepared event). Re-run\n\
     \x20 to replace the candidate; `pr update` invalidates it. A merge\n\
     \x20 conflict fails with E_PREPARE_CONFLICT, naming the conflicted paths\n\
     \x20 and the remedy; no ref changes."
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::{prepare_conflict_error, PrepareConflict};
    use git2::Oid;

    fn conflict_json(paths: &[String]) -> (String, String) {
        let error = prepare_conflict_error(&PrepareConflict {
            id: 7,
            strategy: "squash",
            base_ref: "main",
            source_ref: "ticket",
            base: Oid::from_str("1111111111111111111111111111111111111111").unwrap(),
            source: Oid::from_str("2222222222222222222222222222222222222222").unwrap(),
            paths,
            cleanup: "worktree cleaned up, no ref changes made",
        });
        (error.message.clone(), error.to_json())
    }

    #[test]
    fn prepare_conflict_message_caps_paths_and_data_keeps_all() {
        let paths: Vec<String> = (0..21).map(|i| format!("p{i:02}")).collect();
        let (message, json) = conflict_json(&paths);
        let listed = paths[..20].join(", ");
        assert_eq!(
            message,
            format!(
                "PR #7 cannot be prepared with strategy squash: source \
                 2222222222222222222222222222222222222222 conflicts with base 'main' at \
                 1111111111111111111111111111111111111111 in 21 paths: {listed} (and 1 more); \
                 merge 'main' into the source branch 'ticket', resolve the conflicts, then run \
                 `git forge pr update 7` or create a replacement PR; worktree cleaned up, no ref \
                 changes made"
            )
        );
        let quoted: Vec<String> = paths.iter().map(|p| format!("\"{p}\"")).collect();
        assert!(
            json.ends_with(&format!(
                ",\"data\":{{\"pr\":7,\"strategy\":\"squash\",\"base\":\
                 \"1111111111111111111111111111111111111111\",\"source\":\
                 \"2222222222222222222222222222222222222222\",\"paths\":[{}]}}}}}}",
                quoted.join(",")
            )),
            "{json}"
        );
        assert!(json.starts_with("{\"error\":{\"code\":\"E_PREPARE_CONFLICT\","));
    }

    #[test]
    fn prepare_conflict_message_lists_all_paths_up_to_the_cap() {
        let paths: Vec<String> = (0..20).map(|i| format!("p{i:02}")).collect();
        let (message, _) = conflict_json(&paths);
        assert!(message.contains(&format!("in 20 paths: {};", paths.join(", "))));
        let (message, _) = conflict_json(&["only.txt".to_string()]);
        assert!(message.contains("in 1 path: only.txt;"), "{message}");
    }
}
