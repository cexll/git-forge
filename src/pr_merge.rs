//! PR merge command: approval gate, stale-base check, temporary-worktree
//! strategy execution, and the atomic completion transaction.
//!
//! Split from `cli.rs` so the CLI layer stays under the size gate. Identity is
//! resolved and bound here via [`crate::identity`] before any forge write
//! (VAL-115: never read libgit2 config after `Repository::open`).

use crate::identity::{bind_identity, resolve_identity};
use crate::store::{BoundEventStore, EventStore};

/// `git forge pr merge [<n>] [--squash|--rebase]`
///
/// Fold the PR chain, require effective review decision = approve, then merge
/// the immutable snapshot (`source_head` → `base_ref` tip) via the git binary
/// in a temporary worktree, and atomically finalize: delete pending result
/// ref + CAS base branch + CAS PR head chain in ONE ref transaction.
pub(crate) fn cmd_pr_merge(store: EventStore, args: &[String]) -> Result<String, String> {
    let mut id_arg = None;
    let mut strategy: Option<&str> = None; // merge | squash | rebase
    for a in args {
        match a.as_str() {
            "--merge" => {
                if strategy.is_some() {
                    return Err("merge strategy flag specified more than once".into());
                }
                strategy = Some("merge");
            }
            "--squash" => {
                if strategy.is_some() {
                    return Err("merge strategy flag specified more than once".into());
                }
                strategy = Some("squash");
            }
            "--rebase" => {
                if strategy.is_some() {
                    return Err("merge strategy flag specified more than once".into());
                }
                strategy = Some("rebase");
            }
            "-h" | "--help" => return Ok(pr_merge_help()),
            s if s.starts_with('-') => return Err(format!("unknown option '{s}'")),
            s => {
                if id_arg.is_some() {
                    return Err(
                        "usage: git forge pr merge [<n>] [--merge|--squash|--rebase] \
                                (only one PR id allowed)"
                            .into(),
                    );
                }
                id_arg = Some(s.to_string());
            }
        }
    }
    let strategy = strategy.unwrap_or("merge");
    let id = crate::cli::parse_entity_id(id_arg.as_deref().unwrap_or(""))?;
    let head = crate::store::pr_head_ref(id);
    if store.repo().find_reference(&head).is_err() {
        return Err(format!("PR #{id} does not exist"));
    }
    let chain = store.read_chain(&head).map_err(|e| e.to_string())?;
    let state = crate::event::fold(&chain).pr;

    // Gate: effective approve required (last reachable pr.review).
    if state.effective_decision.as_deref() != Some("approve") {
        return Err(format!(
            "PR #{id} is not approved (effective decision: {})",
            state.effective_decision.as_deref().unwrap_or("none")
        ));
    }
    // Already merged: a pr.merge event exists → no double merge.
    if state.merge_result.is_some() {
        return Err(format!("PR #{id} is already merged"));
    }
    let base_ref = state
        .base_ref
        .clone()
        .ok_or_else(|| "PR has no base_ref".to_string())?;
    let source_oid = store
        .repo()
        .find_reference(&crate::store::pr_source_ref(id))
        .and_then(|r| r.target().ok_or(git2::Error::from_str("no target")))
        .map_err(|_| format!("PR #{id} snapshot source ref missing"))?;
    let base_oid = store
        .repo()
        .find_reference(&crate::store::pr_base_ref(id))
        .and_then(|r| r.target().ok_or(git2::Error::from_str("no target")))
        .map_err(|_| format!("PR #{id} snapshot base ref missing"))?;
    let merge_base = state
        .merge_base
        .as_deref()
        .map(|s| s.parse::<git2::Oid>())
        .transpose()
        .map_err(|_| "PR merge_base invalid".to_string())?
        .ok_or_else(|| "PR has no merge_base".to_string())?;

    // Stale-base rejection: current base branch tip must equal PR base_head.
    let current_base = store
        .repo()
        .find_reference(&format!("refs/heads/{base_ref}"))
        .and_then(|r| {
            r.target()
                .ok_or(git2::Error::from_str("base branch has no target"))
        })
        .map_err(|_| format!("base branch '{base_ref}' does not exist"))?;
    if current_base != base_oid {
        return Err(format!(
            "base branch '{base_ref}' has moved since PR #{id} was created \
             ({current_base} != snapshot {base_oid}); recreate the PR or merge manually"
        ));
    }

    // Temporary worktree, detached at the immutable base OID. worktree
    // add/remove/list are REPOSITORY-level commands: run them from the main
    // worktree, never from the temp dir's parent.
    let repo_dir = store
        .repo()
        .workdir()
        .map(|p| p.to_path_buf())
        .ok_or_else(|| "cannot resolve repository working directory".to_string())?;

    // Checked-out base guard: refuse if base is checked out anywhere.
    if crate::git::base_checked_out_elsewhere(&repo_dir, &base_ref)? {
        return Err(format!(
            "base branch '{base_ref}' is checked out in a worktree; \
             run the merge from/against an un-checked-out base"
        ));
    }

    // Resolve the event actor NOW, before any merge side effects (worktree
    // reservation, pending-result ref, execution). Identity resolution is
    // fallible (config read); resolving it any later — after the pending-result
    // ref exists — could leak that ref on a config error. The signature is
    // bound here too: every forge commit below uses the pre-bound signature,
    // never a libgit2 config read at write time.
    let (signature, actor) = resolve_identity()?;
    let (store, actor) = bind_identity(store, signature, actor);

    // Repo-specific temp worktree path: hash the canonical workdir so parallel
    // test repos (same pid) never collide on the global temp dir.
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    repo_dir.to_string_lossy().as_bytes().hash(&mut hasher);
    let nonce = hasher.finish();
    // Rebase must start detached at /source so `git rebase --onto /base
    // <merge_base>` can replay the snapshot commits; everything else starts
    // detached at the immutable /base OID.
    let detach_oid = if strategy == "rebase" {
        source_oid
    } else {
        base_oid
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
        return Err(format!("failed to create temporary worktree: {err_wt}"));
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
        base_oid,
        merge_base,
        state.title.as_deref().unwrap_or(""),
    )?;
    let result_commit = result_commit.parse::<git2::Oid>().map_err(|_| {
        crate::git::cleanup_failed_worktree(
            &repo_dir,
            &tmp,
            "merge",
            "invalid result commit".into(),
        )
    })?;

    // Keep result_commit reachable across worktree removal (GC could prune the
    // merge commit before the final transaction pins it).
    store
        .create_pending_result_ref(id, result_commit)
        .map_err(|e| {
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
            let pending = cleanup_pending_result(&store, id, result_commit, &mut lock_handle, &tmp);
            return Err(format!(
                "test seam: failed to lock temp worktree for removal-failure injection: \
                 {lock_err} (worktree left at {}{pending})",
                tmp.display()
            ));
        }
    }
    if let Err(err_rm) = crate::git::worktree_remove(&repo_dir, &tmp) {
        // Best-effort: remove the pending result ref (CAS expected OID); the
        // worktree itself stays (can't force-clean a failed removal safely).
        // Report only if the pending ref remains.
        let pending = cleanup_pending_result(&store, id, result_commit, &mut lock_handle, &tmp);
        return Err(format!(
            "merge succeeded but temp worktree removal failed: {err_rm} \
             (worktree left at {}{pending})",
            tmp.display()
        ));
    }
    // `git worktree remove` already removed the directory; do NOT run a
    // recursive delete here (each path is owned by whichever merge holds its
    // sibling lock — deleting another process's worktree is never safe).
    // AC-005h: verify the disposable directory is actually gone.
    if tmp.exists() {
        let pending = cleanup_pending_result(&store, id, result_commit, &mut lock_handle, &tmp);
        return Err(format!(
            "merge succeeded but temp worktree directory still exists at {}{pending}",
            tmp.display()
        ));
    }
    match crate::git::worktree_list_raw(&repo_dir) {
        (false, _, err_l) => {
            // Best-effort: remove the pending result ref; cannot verify the
            // worktree is gone → hard abort before any ref update.
            let pending = cleanup_pending_result(&store, id, result_commit, &mut lock_handle, &tmp);
            return Err(format!(
                "merge succeeded but worktree verification failed (git worktree list: {err_l}){pending}"
            ));
        }
        (true, list_l, _) if list_l.contains(&tmp.to_string_lossy().to_string()) => {
            // Best-effort: remove the pending result ref; the stale registration
            // is reported but the result commit is not left dangling.
            let pending = cleanup_pending_result(&store, id, result_commit, &mut lock_handle, &tmp);
            return Err(format!(
                "merge succeeded but temp worktree still registered at {}{pending}",
                tmp.display()
            ));
        }
        (true, _, _) => {}
    }
    // Worktree removed AND verified gone: release our path lock so a concurrent
    // same-repo merge can reuse the path, then run barrier + final transaction.
    release_lock(&mut lock_handle, &tmp);

    // Test-only pending-window barrier: not part of the user CLI contract.
    #[cfg(debug_assertions)]
    if let Err(b) = maybe_run_test_barrier() {
        // Deadline failed: remove the pending ref best-effort (CAS expected
        // OID); report only if it remains.
        let pending = cleanup_pending_result(&store, id, result_commit, &mut lock_handle, &tmp);
        if !pending.is_empty() {
            return Err(format!(
                "{b} (pending result ref refs/forge/prs/{id}/result left in place)"
            ));
        }
        return Err(b);
    }

    // Single atomic completion transaction: delete pending ref + CAS base + CAS head.
    // finalize_pr_merge reads the head tip itself so all three move atomically.
    store
        .finalize_pr_merge(id, &base_ref, base_oid, result_commit, &actor)
        .map_err(|e| {
            // Nothing moved; report the leftover pending ref.
            format!(
                "merge execution finished but final transaction failed \
                 ({e}); refs unchanged, pending result ref refs/forge/prs/{id}/result left in place"
            )
        })?;
    Ok(format!("merged PR #{id} into {base_ref} ({result_commit})"))
}

/// Shared best-effort pending-result cleanup, used by all six early-return
/// sites in `cmd_pr_merge` (seam worktree-lock failure / removal failed / dir
/// still exists / list failed / still registered / barrier deadline): release
/// the temp-path sibling lock, CAS-delete `refs/forge/prs/<id>/result`
/// (expected `result_commit`), and return the user-facing "pending result ref
/// left in place" suffix (empty when the ref was deleted or already absent).
/// Each call site keeps its own distinct error message string.
fn cleanup_pending_result(
    store: &BoundEventStore,
    id: u64,
    result_commit: git2::Oid,
    lock_handle: &mut Option<std::fs::File>,
    tmp: &std::path::Path,
) -> String {
    // Release the path lock (drop handle + remove file) so a concurrent
    // same-repo merge can reuse the path. Idempotent: runs even at sites
    // where the lock is already released.
    *lock_handle = None;
    let _ = std::fs::remove_file(tmp.with_extension("lock"));
    let left = match store.delete_pending_result_ref(id, result_commit) {
        Ok(true) | Ok(false) => false,
        Err(_) => true,
    };
    if left {
        format!("; pending result ref refs/forge/prs/{id}/result left in place")
    } else {
        String::new()
    }
}

/// Test-only pending-window barrier (wire contract § Test-only pending-window
/// barrier). Debug builds only; inert in release. When
/// `GIT_FORGE_TEST_MERGE_BARRIER=<dir>` is set, the merge pauses after the
/// pending `/result` ref exists (and the temp worktree is gone) but before the
/// final transaction:
///   1. atomically create `<dir>/ready` (O_CREAT|O_EXCL);
///   2. poll for `<dir>/release` (bounded 30s deadline);
///   3. on success, delete `<dir>/release` and continue;
///   4. on deadline, remove both sentinels best-effort and fail the merge with
///      no ref updates.
#[cfg(debug_assertions)]
fn maybe_run_test_barrier() -> Result<(), String> {
    use std::io::Write;
    let Ok(dir) = std::env::var("GIT_FORGE_TEST_MERGE_BARRIER") else {
        return Ok(());
    };
    let dir = std::path::PathBuf::from(dir);
    std::fs::create_dir_all(&dir).map_err(|e| format!("barrier dir: {e}"))?;
    let ready = dir.join("ready");
    let release = dir.join("release");
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&ready)
        .map_err(|e| format!("barrier ready sentinel: {e}"))?;
    writeln!(f, "ready").ok();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while !release.exists() {
        if std::time::Instant::now() > deadline {
            let _ = std::fs::remove_file(&ready);
            let _ = std::fs::remove_file(&release);
            return Err("test merge barrier deadline exceeded; no ref updates made".into());
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    // Observed release: delete it and the ready sentinel, then continue.
    let _ = std::fs::remove_file(&release);
    let _ = std::fs::remove_file(&ready);
    Ok(())
}

fn pr_merge_help() -> String {
    "usage: git forge pr merge [<n>] [--merge|--squash|--rebase]\n\
     \x20 default / --merge: merge commit (--no-ff --no-edit)\n\
     \x20 --squash: single squashed commit\n\
     \x20 --rebase: replay source onto base (linear history)"
        .to_string()
}
