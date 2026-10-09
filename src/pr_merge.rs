//! PR merge command: approval gate, latest-CI-Check gate, stale-base check,
//! temporary-worktree strategy execution, and the atomic completion
//! transaction.
//!
//! Split from `cli.rs` so the CLI layer stays under the size gate. Identity is
//! resolved and bound here via [`crate::identity`] before any forge write
//! (VAL-115: never read libgit2 config after `Repository::open`).

use crate::cli_error::CliError;

use crate::identity::{bind_identity, resolve_identity};

/// `git forge pr diff <n>` — three-dot diff from the immutable snapshot refs.
pub(crate) fn cmd_pr_diff(args: &[String]) -> Result<String, CliError> {
    // Same strict guard as `pr show`: one positional, no options — any
    // dash-leading leftover is a usage error naming the flag (VAL-202).
    let (json, args) = crate::machine::json_requested(args);
    let (id_arg, rest) = args
        .split_first()
        .ok_or_else(|| CliError::usage("usage: git forge pr diff <n> [--json]", ""))?;
    crate::machine::reject_leftover_flags(rest, "usage: git forge pr diff <n>")?;
    let id = crate::cli::parse_entity_id(id_arg)?;
    let store = &crate::cli::open_store()?;
    let head = crate::store::pr_head_ref(id);
    if !crate::cli::store_has_ref(store, &head) {
        return Err(CliError::not_found(
            format!("PR #{id} does not exist"),
            id.to_string(),
        ));
    }
    // Resolve from the CURRENT generation's immutable snapshot refs (survive
    // branch deletion + gc): generation 0 uses the L1 `/source`//`/base`
    // refs, later generations their `/source/g<k>`//`/base/g<k>` pins. The
    // folded head chain is the authority on which generation is current.
    let head_tip = store
        .repo()
        .find_reference(&head)
        .and_then(|r| r.target().ok_or(git2::Error::from_str("no target")))
        .map_err(|_| CliError::not_found(format!("PR #{id} does not exist"), id.to_string()))?;
    let (anchored, chain) = store
        .read_pr_chain_anchored(id, head_tip)
        .map_err(CliError::from)?;
    if !anchored {
        return Err((format!("PR #{id} head chain is not anchored to PR #{id}")).into());
    }
    let state = crate::event::fold(&chain).pr;
    let source = crate::store::pr_source_ref_for(id, state.generation);
    let base = crate::store::pr_base_ref_for(id, state.generation);
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
    // F-008: a retargeted `/source`//`/base` would let `pr diff` display a
    // different patch than the one a reviewer approves (shared invariant,
    // one owner in the store).
    crate::store::EventStore::validate_snapshot_against_state(id, &state, source_oid, base_oid)?;
    // F-027 (review): the `git diff` shell-out must run through the SANITIZED
    // git adapter — a caller `GIT_EXTERNAL_DIFF`/`GIT_DIR`/`GIT_CONFIG` would
    // otherwise render an arbitrary/redirected patch (or run an external-diff
    // program), so a reviewer could approve a different display than the
    // validated snapshot.
    // `--no-ext-diff`/`--no-textconv` also disable a REPO-LOCAL `diff.external`
    // or an attribute `textconv` driver, which `git_in_ci` intentionally retains
    // (the repo's own config) and which would otherwise execute arbitrary code
    // during `pr diff`. The repo is passed as its WORKTREE root (not the `.git`
    // path) so `-c safe.directory=<root>` matches the path Git's ownership check
    // actually validates for a non-bare repository.
    let repo_dir = store
        .repo()
        .workdir()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| store.repo().path().to_path_buf());
    let (ok, out, err) = crate::git::git_in_ci(
        &repo_dir,
        &[
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            &format!("{base_oid}...{source_oid}"),
        ],
    );
    if !ok {
        return Err((if err.is_empty() {
            "git diff failed".to_string()
        } else {
            err
        })
        .into());
    }
    if json {
        return Ok(crate::machine::pr_diff_json(
            &state,
            &base_oid.to_string(),
            &source_oid.to_string(),
            &out,
        ));
    }
    Ok(out)
}

/// `git forge pr merge [<n>] [--merge|--squash|--rebase|--ff]`
///
/// Fold the PR chain, require effective review decision = approve AND the
/// latest CI Check = success, then merge the immutable snapshot
/// (`source_head` → `base_ref` tip) via the git binary in a temporary
/// worktree, and atomically finalize: delete pending result ref + CAS base
/// branch + CAS PR head chain in ONE ref transaction.
//
/// Value-taking options of `pr merge`.
pub(crate) const MERGE_VALUES: &[&str] = &["--recycle-worktree"];

// Baseline exemption (removal trigger in constraints.yaml baseline.lint_allow):
// the sole lib/src function over the 150-line clippy::too_many_lines threshold.
#[allow(clippy::too_many_lines)]
pub(crate) fn cmd_pr_merge(args: &[String]) -> Result<String, CliError> {
    let (json, args) = crate::machine::json_requested_with(args, MERGE_VALUES);
    let render = |outcome: crate::merge_state::MergeOutcome| {
        if json {
            crate::machine::pr_merge_json(&outcome)
        } else {
            outcome.prose()
        }
    };
    let mut id_arg = None;
    let mut strategy: Option<&str> = None; // merge | squash | rebase
    let mut recycle: Option<String> = None;
    let mut i = 0;
    let mut force = false;
    while i < args.len() {
        match args[i].as_str() {
            "--recycle-worktree" => {
                // #4: the next token is the source-worktree path to recycle.
                i += 1;
                if i >= args.len() {
                    return Err(CliError::usage(
                        "--recycle-worktree requires a worktree path",
                        "",
                    ));
                }
                if recycle.is_some() {
                    return Err(CliError::usage(
                        "--recycle-worktree specified more than once",
                        "",
                    ));
                }
                recycle = Some(args[i].clone());
            }
            "--merge" => {
                if strategy.is_some() {
                    return Err(CliError::usage(
                        "merge strategy flag specified more than once",
                        "",
                    ));
                }
                strategy = Some("merge");
            }
            "--squash" => {
                if strategy.is_some() {
                    return Err(CliError::usage(
                        "merge strategy flag specified more than once",
                        "",
                    ));
                }
                strategy = Some("squash");
            }
            "--rebase" => {
                if strategy.is_some() {
                    return Err(CliError::usage(
                        "merge strategy flag specified more than once",
                        "",
                    ));
                }
                strategy = Some("rebase");
            }
            "--ff" => {
                if strategy.is_some() {
                    return Err(CliError::usage(
                        "merge strategy flag specified more than once",
                        "",
                    ));
                }
                strategy = Some("ff");
            }
            "--force" => {
                // #4: unlocks ONLY the dirty-source-worktree recycle refusal.
                force = true;
            }
            h @ ("-h" | "--help") => return crate::cli::help_result(json, h, pr_merge_help()),
            s if s.starts_with('-') => {
                return Err(CliError::usage(format!("unknown option '{s}'"), s))
            }
            s => {
                if id_arg.is_some() {
                    return Err(CliError::usage("usage: git forge pr merge [<n>] [--merge|--squash|--rebase] (only one PR id allowed)", s));
                }
                id_arg = Some(s.to_string());
            }
        }
        i += 1;
    }
    let strategy = strategy.unwrap_or("merge");
    let id = crate::cli::parse_entity_id(id_arg.as_deref().ok_or_else(|| {
        CliError::usage(
            if args.is_empty() {
                "usage: git forge pr merge <n> [--merge|--squash|--rebase|--ff]"
            } else {
                "empty entity id"
            },
            "",
        )
    })?)?;
    // `--force` only unlocks a dirty recycle target: a pure argument check,
    // decided before the repository is opened.
    if force && recycle.is_none() {
        return Err(CliError::usage("--force requires --recycle-worktree", ""));
    }
    let store = crate::cli::open_store()?;
    let head = crate::store::pr_head_ref(id);
    // Bind the gate decision to the EXACT PR-head OID it validated (F-007):
    // the completion transaction CASes the head chain from OID `gate_head`,
    // never from a freshly reread tip, so a concurrent `ci run` that appends a
    // failed Check during merge execution can neither slip past the gate nor
    // be silently accepted at finalize.
    if !crate::cli::store_has_ref(&store, &head) {
        return Err(CliError::not_found(
            format!("PR #{id} does not exist"),
            id.to_string(),
        ));
    }
    let (state, gate_head) = crate::merge_state::read_merge_gate(&store, id, &head)?;
    let base_ref = state
        .base_ref
        .clone()
        .ok_or_else(|| "PR has no base_ref".to_string())?;
    let source_oid = store
        .repo()
        .find_reference(&crate::store::pr_source_ref_for(id, state.generation))
        .and_then(|r| r.target().ok_or(git2::Error::from_str("no target")))
        .map_err(|_| format!("PR #{id} snapshot source ref missing"))?;
    let base_oid = store
        .repo()
        .find_reference(&crate::store::pr_base_ref_for(id, state.generation))
        .and_then(|r| r.target().ok_or(git2::Error::from_str("no target")))
        .map_err(|_| format!("PR #{id} snapshot base ref missing"))?;
    // The current generation's immutable-snapshot refs must still point at
    // the AUTHORITATIVE snapshot carried by the gate state. The comparison
    // uses the ALREADY-ANCHORED gate state (F-008), NOT a fresh `/head`
    // read — a concurrent rewrite between the gate read and the finalize
    // cannot substitute a forged snapshot (shared invariant, one owner in
    // the store).
    crate::store::EventStore::validate_snapshot_against_state(id, &state, source_oid, base_oid)?;
    // ── #2 live-base merge ── The base branch's CURRENT tip. When it differs
    // from the snapshot base_head the merge is NOT refused outright:
    // merge_base is recomputed against this tip, the strategy executes
    // against it, and the finalize CAS expectation is THIS tip — the
    // gate-validated current base tip, the base-side counterpart of the
    // gate_head binding (F-007/F-008 binding redesign, named below).
    let current_base = store
        .repo()
        .find_reference(&format!("refs/heads/{base_ref}"))
        .and_then(|r| {
            r.target()
                .ok_or(git2::Error::from_str("base branch has no target"))
        })
        .map_err(|_| format!("base branch '{base_ref}' does not exist"))?;
    let moved = current_base != base_oid;
    // F-002 prepared-candidate routing: the merge of a moved base (or any
    // candidate-pinned PR) binds the PREPARED candidate — result committed
    // at prepare time, never re-materialized. When a current, validated
    // candidate exists, the three checks below (candidate current + strategy
    // match + every gate input bound to the candidate) replace the old
    // result-bound gate; when the base has MOVED and no candidate exists,
    // the merge refuses naming the `pr prepare` remedy (the legacy strategy
    // re-execution path cannot bind a review to a result it never saw);
    // when the base has NOT moved and no candidate exists, the legacy
    // path is preserved exactly (the transient result ref lifecycle keeps
    // its existing contract).
    let candidate = match &state.prepared {
        Some(c) if c.generation == state.generation => Some(c),
        _ => None,
    };
    if let Some(c) = candidate {
        crate::pr_prepare::validate_prepared_candidate(
            id,
            &state,
            c,
            &base_ref,
            current_base,
            strategy,
        )?;
    } else if moved {
        return Err(CliError::new(
            "E_GATE",
            format!(
                "PR #{id} base branch '{base_ref}' has moved since the PR snapshot \
                 ({current_base} != snapshot {base_oid}); prepare the merge result \
                 first: run `git forge pr prepare {id} --strategy {strategy}`, then \
                 `git forge ci run {id}` and `git forge pr review {id} --approve`, \
                 then merge"
            ),
            id.to_string(),
            "Inspect PR review and CI state and satisfy the reported merge gate.",
            false,
        ));
    } else if state.prepared.is_some() {
        // A STALE prepared candidate (e.g. superseded by `pr update`) still
        // pins `refs/forge/prs/<n>/result`; the legacy path's
        // create_pending_result_ref expects absence, so refuse EARLY with
        // the re-prepare remedy instead of a confusing mid-merge failure.
        // (A manually-created ref with no prepared event keeps the legacy
        // create-time refusal — that path is unchanged.)
        return Err(CliError::new(
            "E_GATE",
            format!(
                "PR #{id} has a stale prepared candidate (prepared on generation {}, \
                 current snapshot generation {}); re-run `git forge pr prepare {id} \
                 --strategy {strategy}`, then `git forge ci run {id}` and `git forge pr \
                 review {id} --approve`, then merge",
                state.prepared.as_ref().map(|c| c.generation).unwrap_or(0),
                state.generation
            ),
            id.to_string(),
            "Inspect PR review and CI state and satisfy the reported merge gate.",
            false,
        ));
    }
    // merge_base recomputed against the CURRENT base tip. When the base has
    // not moved the recomputation is deterministic over the snapshot OIDs,
    // so one code path serves both the unmoved and the live-base merge.
    let exec_merge_base =
        crate::git::require_single_merge_base(store.repo(), source_oid, current_base)?;
    // --ff precondition (VAL-102): the source must sit directly on top of
    // the CURRENT base tip (recomputed merge_base == base tip), whether or
    // not the base has moved since the snapshot. A moved base is revalidated,
    // never blanket-refused (supersedes the pre-#2 VAL-103 refusal).
    if strategy == "ff" && exec_merge_base != current_base {
        return Err((format!(
            "PR #{id} cannot fast-forward: the source does not sit directly on \
             top of the current base tip of '{base_ref}' (merge_base \
             {exec_merge_base} != base tip {current_base}); use \
             `pr merge --rebase` to replay the source or recreate the PR"
        ))
        .into());
    }

    // Temporary worktree, detached at the immutable base OID. worktree
    // add/remove/list are REPOSITORY-level commands: run them from the main
    // worktree, never from the temp dir's parent.
    let repo_dir = store
        .repo()
        .workdir()
        .map(|p| p.to_path_buf())
        .ok_or_else(|| "cannot resolve repository working directory".to_string())?;

    // Checked-out base guard (#3 live-base refresh): the base may be checked
    // out in non-temporary worktrees, but ONLY while every such worktree is
    // clean — `git status --porcelain` empty (untracked counts as dirty;
    // ignored does not). A dirty worktree refuses BEFORE any side effect:
    // advancing a checked-out branch without updating its working tree would
    // leave files at the old tree, and no `--force` unlocks this refusal.
    let base_worktrees = crate::git::base_checked_out_worktrees(&repo_dir, &base_ref)?;
    for wt in &base_worktrees {
        if crate::git::worktree_dirty(wt)? {
            return Err((format!(
                "base branch '{base_ref}' is checked out in a dirty worktree ({}); \
                 commit, stash or clean it (including untracked files) before merging",
                wt.display()
            ))
            .into());
        }
    }
    // Recycle pre-validation (#4, phase 1): a refused recycle must leave the
    // merge UNEXECUTED (PR unmerged, refs unchanged), so containment (the
    // path must be a registered worktree on the PR source branch) and the
    // dirty check (--force is the only unlock) run HERE, before any merge
    // side effect — refused-not-contained / refused-dirty exit non-zero with
    // nothing touched.
    if let Some(target) = &recycle {
        let source_ref = state
            .source_ref
            .as_deref()
            .ok_or("--recycle-worktree requires a PR with a source branch")?;
        crate::merge_state::validate_recycle_target(&repo_dir, source_ref, target, force)?;
    }
    // Resolve the event actor NOW, before any merge side effects (worktree
    // reservation, pending-result ref, execution). Identity resolution is
    // fallible (config read); resolving it any later — after the pending-result
    // ref exists — could leak that ref on a config error. The signature is
    // bound here too: every forge commit below uses the pre-bound signature,
    // never a libgit2 config read at write time.
    let (signature, actor) = resolve_identity()?;
    let (store, actor) = bind_identity(store, signature, actor);

    // ── F-002 prepared candidate path ── The candidate was materialized at
    // prepare time and is already pinned (`refs/forge/prs/<n>/result`). The
    // merge binds it directly: no new worktree, no re-materialization, no
    // verify_result_ci (the candidate-bound CI Check already validated the
    // result tree). The finalize CASes the base FROM `candidate.base_head`
    // (the gate-validated current base tip) TO `candidate.result_commit`.
    if let Some(c) = candidate {
        // Never-split posture: the merged base_worktrees check already ran.
        // If the pinned result ref VANISHED (concurrent cleanup for another
        // finalize or a stale leftover), refuse — the candidate identity is
        // corroborated by the pin, and its absence is unexplained state.
        let pin = store
            .repo()
            .find_reference(&crate::store::pr_result_ref(id))
            .and_then(|r| r.target().ok_or(git2::Error::from_str("no target")))
            .map_err(|_| {
                format!("PR #{id} prepared candidate ref missing (refs/forge/prs/{id}/result)")
            })?;
        if pin.to_string() != c.result_commit {
            return Err((format!(
                "PR #{id} prepared candidate ref refs/forge/prs/{id}/result does not \
                 point at the folded candidate ({pin} != {}); re-run \
                 `git forge pr prepare {id} --strategy {strategy}`",
                c.result_commit
            ))
            .into());
        }
        let result_commit = c
            .result_commit
            .parse::<git2::Oid>()
            .map_err(|_| "PR prepared candidate result_commit invalid".to_string())?;
        // The candidate result must exist as an object (a garbled pin would
        // otherwise fail at finalize; refuse EARLY with nothing touched).
        store.repo().find_commit(result_commit).map_err(|_| {
            format!("PR #{id} prepared candidate result {result_commit} is not a known commit")
        })?;
        // The candidate was built against `candidate.base_head` = current
        // base tip (validated above by validate_prepared_candidate); the
        // finalize CAS uses that exact tip.
        let gate_base_oid = c
            .base_head
            .parse::<git2::Oid>()
            .map_err(|_| "PR prepared candidate base_head invalid".to_string())?;
        if gate_base_oid != current_base {
            return Err((format!(
                "PR #{id} prepared candidate base change during merge ({gate_base_oid} != \
                 current {current_base}); re-run `git forge pr prepare {id} --strategy \
                 {strategy}`"
            ))
            .into());
        }
        // No temp worktree on the candidate path; pass a unique temp path so
        // the failure-path cleanup's lock-file removal is a no-op on a path
        // that never exists (never a user file).
        let prepared_tmp = std::env::temp_dir().join(format!(
            "git-forge-pr{id}-prepared-finalize-{}",
            std::process::id()
        ));
        let mut no_lock: Option<std::fs::File> = None;
        #[cfg(debug_assertions)]
        if let Err(b) = crate::merge_state::maybe_run_test_barrier() {
            return Err((format!(
                "{b} (pending result ref refs/forge/prs/{id}/result left in place)"
            ))
            .into());
        }
        return crate::merge_state::run_finalize(
            &store,
            crate::merge_state::MergeFinalizeSpec {
                id,
                base_ref: &base_ref,
                gate_base_oid,
                result_commit,
                actor: &actor,
                strategy,
                gate_head,
                closes_issue: state.closes,
                base_worktrees: &base_worktrees,
                recycle: recycle.as_deref().map(|p| (p, force)),
                source_ref: state.source_ref.as_deref(),
                source_head: &c.source_head,
                generation: state.generation,
                prepared_result: Some(&c.result_commit),
            },
            &mut no_lock,
            &prepared_tmp,
        )
        .map(render);
    }

    // Repo-specific temp worktree path: hash the canonical workdir so parallel
    // test repos (same pid) never collide on the global temp dir.
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    repo_dir.to_string_lossy().as_bytes().hash(&mut hasher);
    let nonce = hasher.finish();
    // Rebase must start detached at /source so `git rebase --onto /base
    // <merge_base>` can replay the snapshot commits; everything else starts
    // detached at the base OID the strategy executes against — the CURRENT
    // base tip (#2 live-base), identical to the immutable snapshot base when
    // the base has not moved.
    let detach_oid = if strategy == "rebase" {
        source_oid
    } else {
        current_base
    };
    // Reserve the temp path atomically via a SIBLING lock file (`create_new`,
    // O_EXCL): two concurrent merges on the same repo compute the same path,
    // so exactly one wins the lock and the loser retries with a fresh suffix.
    // `git worktree add` is then free to create the directory itself. We never
    // touch a path we do not own.
    let mut attempt = 0u32;
    let mk = |attempt: u32| {
        std::env::temp_dir().join(format!(
            "git-forge-pr{id}-merge-{nonce:x}-{}-{attempt}",
            std::process::id()
        ))
    };
    let mut tmp = mk(0);
    // The lock handle is held until the worktree is removed AND verified gone,
    // so no concurrent same-repo merge can reuse this path while we own it.
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
                match crate::git::worktree_add(&repo_dir, &tmp, &detach_oid) {
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
        // Release our own lock; the path was never registered to us.
        drop(lock_handle.take());
        let _ = std::fs::remove_file(tmp.with_extension("lock"));
        return Err((format!("failed to create temporary worktree: {err_wt}")).into());
    }

    // Release the path lock (drop handle + remove file) so a concurrent
    // same-repo merge can reuse the path. Must run on EVERY early return after
    // the worktree was added (strategy failures go through cleanup_failed_worktree
    // which also drops the file; removal/list/barrier/finalize returns call this).
    let release_lock = |lock_handle: &mut Option<std::fs::File>, path: &std::path::Path| {
        *lock_handle = None;
        let _ = std::fs::remove_file(path.with_extension("lock"));
    };

    // Execute the strategy inside the worktree. result_commit = worktree HEAD.
    // The squash path commits with the PR title, checked INSIDE the adapter
    // AFTER `git merge --squash` staged the changes (pre-extraction ordering,
    // VAL-101: a missing title produces the cleanup error after staging); the
    // rebase/merge paths never use the title. All git shell-outs run in the
    // adapter.
    let result_commit = crate::git::execute_strategy(
        &repo_dir,
        &tmp,
        strategy,
        source_oid,
        current_base,
        exec_merge_base,
        state.title.as_deref().unwrap_or(""),
    )
    .map_err(|error| CliError::from(error.message))?;
    let result_commit = result_commit.parse::<git2::Oid>().map_err(|_| {
        crate::git::cleanup_failed_worktree(
            &repo_dir,
            &tmp,
            "merge",
            "invalid result commit".into(),
        )
    })?;

    // ── #2 identify what CI verified with the result ── The fresh Check
    // validated the SOURCE snapshot tree; the RESULT tree is what lands.
    if let Err(ci_err) =
        crate::merge_state::verify_result_ci(store.repo(), &tmp, source_oid, result_commit)
    {
        return Err(
            (crate::git::cleanup_failed_worktree(&repo_dir, &tmp, strategy, ci_err)).into(),
        );
    }

    // Keep result_commit reachable across worktree removal (GC could prune the
    // merge commit before the final transaction pins it).
    crate::merge_state::create_pending_result_ref(&store, id, result_commit).map_err(|e| {
        crate::git::cleanup_failed_worktree(&repo_dir, &tmp, "merge", e.to_string())
    })?;

    // Remove the temporary worktree, then verify it is gone. worktree
    // remove/list run from the repository.
    // Debug-only test seam (VAL-027): lock the temp worktree so
    // `git worktree remove --force` fails deterministically, exercising the
    // removal-failure branch (leftover report + best-effort pending-ref
    // cleanup). Inert in release builds and when the env var is unset.
    #[cfg(debug_assertions)]
    if std::env::var("GIT_FORGE_TEST_FAIL_WORKTREE_REMOVE").as_deref() == Ok("1") {
        if let Err(lock_err) = crate::git::worktree_lock(&repo_dir, &tmp) {
            // F-019: the seam failed before worktree removal. The pending
            // result ref exists and the temp worktree is present, so route
            // through the SAME cleanup as the removal-failure branch —
            // release the sibling path lock and best-effort remove the
            // pending ref — before returning, so no lock/ref leak survives
            // even in this injected path.
            let pending = crate::merge_state::cleanup_pending_result(
                &store,
                id,
                result_commit,
                &mut lock_handle,
                &tmp,
            );
            return Err((format!(
                "test seam: failed to lock temp worktree for removal-failure injection: \
                 {lock_err} (worktree left at {}{pending})",
                tmp.display()
            ))
            .into());
        }
    }
    if let Err(err_rm) = crate::git::worktree_remove(&repo_dir, &tmp) {
        // Best-effort: remove the pending result ref (CAS expected OID); the
        // worktree itself stays (can't force-clean a failed removal safely).
        // Report only if the pending ref remains.
        let pending = crate::merge_state::cleanup_pending_result(
            &store,
            id,
            result_commit,
            &mut lock_handle,
            &tmp,
        );
        return Err((format!(
            "merge succeeded but temp worktree removal failed: {err_rm} \
             (worktree left at {}{pending})",
            tmp.display()
        ))
        .into());
    }
    // `git worktree remove` already removed the directory; do NOT run a
    // recursive delete here (each path is owned by whichever merge holds its
    // sibling lock — deleting another process's worktree is never safe).
    // AC-005h: verify the disposable directory is actually gone.
    if tmp.exists() {
        let pending = crate::merge_state::cleanup_pending_result(
            &store,
            id,
            result_commit,
            &mut lock_handle,
            &tmp,
        );
        return Err((format!(
            "merge succeeded but temp worktree directory still exists at {}{pending}",
            tmp.display()
        ))
        .into());
    }
    match crate::git::worktree_list_raw(&repo_dir) {
        (false, _, err_l) => {
            // Best-effort: remove the pending result ref; cannot verify the
            // worktree is gone → hard abort before any ref update.
            let pending = crate::merge_state::cleanup_pending_result(
                &store,
                id,
                result_commit,
                &mut lock_handle,
                &tmp,
            );
            return Err((format!(
                "merge succeeded but worktree verification failed (git worktree list: {err_l}){pending}"
            )).into());
        }
        (true, list_l, _) if crate::git::worktree_registered_path(&list_l, &tmp) => {
            // Best-effort: remove the pending result ref; the stale registration
            // is reported but the result commit is not left dangling.
            let pending = crate::merge_state::cleanup_pending_result(
                &store,
                id,
                result_commit,
                &mut lock_handle,
                &tmp,
            );
            return Err((format!(
                "merge succeeded but temp worktree still registered at {}{pending}",
                tmp.display()
            ))
            .into());
        }
        (true, _, _) => {}
    }
    // Worktree removed AND verified gone: release our path lock so a concurrent
    // same-repo merge can reuse the path, then run barrier + final transaction.
    release_lock(&mut lock_handle, &tmp);

    // Test-only pending-window barrier: not part of the user CLI contract.
    #[cfg(debug_assertions)]
    if let Err(b) = crate::merge_state::maybe_run_test_barrier() {
        // Deadline failed: remove the pending ref best-effort (CAS expected
        // OID); report only if it remains.
        let pending = crate::merge_state::cleanup_pending_result(
            &store,
            id,
            result_commit,
            &mut lock_handle,
            &tmp,
        );
        if !pending.is_empty() {
            return Err((format!(
                "{b} (pending result ref refs/forge/prs/{id}/result left in place)"
            ))
            .into());
        }
        return Err((b).into());
    }
    crate::merge_state::run_finalize(
        &store,
        crate::merge_state::MergeFinalizeSpec {
            id,
            base_ref: &base_ref,
            // The gate-validated CURRENT base tip (F-007/F-008 binding
            // redesign, base side): the finalize CASes refs/heads/<base_ref>
            // FROM this tip, never from the snapshot base_head and never
            // from a re-read tip.
            gate_base_oid: current_base,
            result_commit,
            actor: &actor,
            strategy,
            gate_head,
            closes_issue: state.closes,
            base_worktrees: &base_worktrees,
            recycle: recycle.as_deref().map(|p| (p, force)),
            source_ref: state.source_ref.as_deref(),
            source_head: &source_oid.to_string(),
            generation: state.generation,
            prepared_result: None,
        },
        &mut lock_handle,
        &tmp,
    )
    .map(render)
}

/// `git forge pr update <n>` — re-pin the PR to the source branch tip as a
/// NEW immutable snapshot generation (t1). The `pr.update` event records
/// old/new source_head, base_head, merge_base; generation-addressable refs
/// `refs/forge/prs/<n>/source/g<k>` / `base/g<g>` pin the new snapshot
/// atomically with the `/head` chain append. Refusals (before any write):
/// unknown PR, abandoned (terminal), already merged, base branch moved (the
/// recreate-or-revalidate remedy stays with #2 live-base), and a branch tip
/// that already IS the snapshot source_head (nothing to update).
pub(crate) fn cmd_pr_update(args: &[String]) -> Result<String, CliError> {
    const USAGE: &str = "usage: git forge pr update <n>";
    let (json, args) = crate::machine::json_requested(args);
    if args.len() != 1 {
        // Name the offending token so a rejected flag is diagnosable
        // (unknown flag keeps exit-1, VAL-202).
        return Err(CliError::usage(
            match args.first() {
                Some(a) if a.starts_with('-') => format!("unknown option '{a}'; {USAGE}"),
                _ => USAGE.into(),
            },
            args.first().map(String::as_str).unwrap_or(""),
        ));
    }
    let id = crate::cli::parse_entity_id(args.first().map(|s| s.as_str()).unwrap_or(""))?;
    // Read-only refusals first: every terminal/consistency check resolves
    // nothing and writes nothing; the transaction below re-opens the store
    // bound to the actor.
    let store = crate::cli::open_store()?;
    let head = crate::store::pr_head_ref(id);
    if !crate::cli::store_has_ref(&store, &head) {
        return Err(CliError::not_found(
            format!("PR #{id} does not exist"),
            id.to_string(),
        ));
    }
    let chain = store.read_chain(&head).map_err(CliError::from)?;
    let state = crate::event::fold(&chain).pr;
    if let Some(reason) = &state.abandoned {
        return Err(CliError::invalid(format!(
            "PR #{id} is abandoned: {reason}"
        )));
    }
    if state.merge_result.is_some() {
        return Err(CliError::invalid(format!("PR #{id} is already merged")));
    }
    let source_ref = state
        .source_ref
        .clone()
        .ok_or_else(|| "PR has no source_ref".to_string())?;
    let base_ref = state
        .base_ref
        .clone()
        .ok_or_else(|| "PR has no base_ref".to_string())?;
    let old_source = state
        .source_head
        .as_deref()
        .map(|s| s.parse::<git2::Oid>())
        .transpose()
        .map_err(|_| "PR source_head invalid".to_string())?
        .ok_or_else(|| "PR has no source_head".to_string())?;
    let old_base = state
        .base_head
        .as_deref()
        .map(|s| s.parse::<git2::Oid>())
        .transpose()
        .map_err(|_| "PR base_head invalid".to_string())?
        .ok_or_else(|| "PR has no base_head".to_string())?;
    let old_merge_base = state
        .merge_base
        .as_deref()
        .map(|s| s.parse::<git2::Oid>())
        .transpose()
        .map_err(|_| "PR merge_base invalid".to_string())?
        .ok_or_else(|| "PR has no merge_base".to_string())?;
    // Re-pin to the LIVE branch tips. The source branch may have moved (the
    // whole point of update); the base branch may NOT — update refuses a
    // moved base with the gated remedies (prepare or replace the PR) rather
    // than silently adopting a base the PR was never reviewed against.
    let resolve_tip = |branch: &str| -> Result<git2::Oid, CliError> {
        store
            .repo()
            .find_reference(&format!("refs/heads/{branch}"))
            .and_then(|r| r.target().ok_or(git2::Error::from_str("no target")))
            .map_err(|_| CliError::not_found(format!("branch '{branch}' does not exist"), branch))
    };
    let new_source = resolve_tip(&source_ref)?;
    let new_base = resolve_tip(&base_ref)?;
    if new_base != old_base {
        return Err(base_moved_error(id, &base_ref, old_base, new_base));
    }
    if new_source == old_source {
        return Err(CliError::invalid(format!("PR #{id} source branch tip is already the snapshot source_head ({old_source}); nothing to update")));
    }
    let new_merge_base = crate::git::require_single_merge_base(store.repo(), new_base, new_source)?;
    let (store, actor) = crate::identity::open_mutation_store()?;
    let generation = state.generation + 1;
    let event = store
        .update_pr(
            id,
            &base_ref,
            old_source,
            old_base,
            old_merge_base,
            new_source,
            new_base,
            new_merge_base,
            generation,
            &actor,
        )
        // The transaction verifies the base tip checked above: a base that
        // moved or was deleted since then is reported as such.
        .map_err(|error| match resolve_tip(&base_ref) {
            Ok(current) if current == new_base => CliError::from(error),
            Ok(current) => base_moved_error(id, &base_ref, old_base, current),
            Err(missing) => missing,
        })?;
    if json {
        return Ok(crate::machine::pr_update_json(
            id,
            crate::machine::pr_state_at(store.store(), event)?,
            generation,
            &new_source.to_string(),
            &new_base.to_string(),
            &new_merge_base.to_string(),
            &event.to_string(),
        ));
    }
    Ok(format!(
        "PR #{id} updated to snapshot generation {generation} \
         (source {source_ref} -> {new_source})"
    ))
}

/// The `pr update` moved-base refusal (`E_BASE_MOVED`): the gated remedies
/// (spec #33) are to prepare a candidate for the existing snapshot or to
/// replace the PR. `<s>` stays literal because update has no strategy to
/// name.
fn base_moved_error(id: u64, base_ref: &str, old_base: git2::Oid, current: git2::Oid) -> CliError {
    let shown = crate::cli::sanitize_terminal(base_ref);
    let mut error = CliError::new(
        "E_BASE_MOVED",
        format!(
            "base branch '{shown}' has moved since PR #{id} was created; run git forge pr \
             prepare {id} --strategy <s> to merge the existing snapshot, or create a \
             replacement PR from the updated source with the same --closes and abandon \
             PR #{id}"
        ),
        base_ref,
        "Inspect the current base and PR snapshot before proceeding.",
        false,
    );
    error.data = vec![
        ("pr", crate::event::JsonValue::Number(id as i64)),
        (
            "base",
            crate::event::JsonValue::String(base_ref.to_string()),
        ),
        (
            "snapshot_base",
            crate::event::JsonValue::String(old_base.to_string()),
        ),
        (
            "current_base",
            crate::event::JsonValue::String(current.to_string()),
        ),
    ];
    error
}

pub(crate) fn pr_merge_help() -> String {
    "usage: git forge pr merge [<n>] [--merge|--squash|--rebase|--ff]\n\
     \x20                   [--recycle-worktree <path>] [--force] [--json]\n\
     \x20 default / --merge: merge commit (--no-ff --no-edit)\n\
     \x20 --squash: single squashed commit\n\
     \x20 --rebase: replay source onto base (linear history)\n\
     \x20 --ff: fast-forward base to source (requires source directly on base)\n\
     \x20 --recycle-worktree <path>: after the merge, remove the registered source\n\
     \x20   worktree whose checked-out branch is the PR source_ref and delete that\n\
     \x20   branch when its tip is the merged source head and an ancestor of the\n\
     \x20   result commit (--squash/--rebase keep it). Refusals (a path that is not\n\
     \x20   such a worktree, or a dirty one without --force) leave the merge\n\
     \x20   unexecuted. Outcome vocabulary: removed | refused-dirty |\n\
     \x20   refused-not-contained | partial-leftover(<path>) | branch-deleted |\n\
     \x20   branch-kept(<reason>)\n\
     \x20 --force: recycle a dirty source worktree anyway (never unlocks the base\n\
     \x20   dirty-worktree refusal)"
        .to_string()
}
