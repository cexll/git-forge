//! Git adapter: every merge-execution shell-out to the `git` binary.
//!
//! Owns the git plumbing behind `git forge pr merge` plus the single
//! merge-base check at PR creation: running git in a directory, temporary
//! worktree add/remove/list (and the debug-only lock test seam), strategy
//! execution (`merge --no-ff --no-edit` / `merge --squash` + `commit -m` /
//! `rebase --onto`), and failure cleanup (`merge --abort` / `rebase --abort`
//! only when state exists / `reset --hard` for squash, `git clean -fd`).
//! `cmd_pr_diff`'s `git diff` shell-out and the merge-gate predicate stay in
//! `cli.rs` (orchestration layer).

use std::path::Path;

/// Run `git` in the given working directory, returning (success, stdout,
/// stderr). Used for worktree merge/rebase execution and cleanup.
fn git_in(dir: &Path, args: &[&str]) -> (bool, String, String) {
    use std::process::Command;
    let out = Command::new("git").arg("-C").arg(dir).args(args).output();
    match out {
        Ok(o) => (
            o.status.success(),
            String::from_utf8_lossy(&o.stdout).trim().to_string(),
            String::from_utf8_lossy(&o.stderr).trim().to_string(),
        ),
        Err(e) => (false, String::new(), format!("failed to spawn git: {e}")),
    }
}

/// True if `git rebase` is mid-flight in the worktree (either rebase-merge or
/// rebase-apply state dir exists, resolved via `git rev-parse --git-path`).
fn rebase_in_progress(dir: &Path) -> bool {
    for sub in ["rebase-merge", "rebase-apply"] {
        let (ok, out, _) = git_in(dir, &["rev-parse", "--git-path", sub]);
        if ok {
            let p = out.trim();
            if !p.is_empty() {
                let abs = if p.starts_with('/') || dir.is_absolute() {
                    let base = Path::new(p);
                    if base.is_absolute() {
                        base.to_path_buf()
                    } else {
                        dir.join(base)
                    }
                } else {
                    dir.join(p)
                };
                if abs.exists() {
                    return true;
                }
            }
        }
    }
    false
}

/// Add a detached temporary worktree at `detach_oid`. worktree add/remove/
/// list are REPOSITORY-level commands: run them from the main worktree,
/// never from the temp dir's parent. Err carries git's stderr.
pub(crate) fn worktree_add(
    repo_dir: &Path,
    path: &Path,
    detach_oid: &git2::Oid,
) -> Result<(), String> {
    let (ok, _, err) = git_in(
        repo_dir,
        &[
            "worktree",
            "add",
            "--detach",
            path.to_str().unwrap_or("/tmp/none"),
            &detach_oid.to_string(),
        ],
    );
    if ok {
        Ok(())
    } else {
        Err(err)
    }
}

/// Remove the disposable temp worktree (`git worktree remove --force`), from
/// the repository. Only ever used for the disposable merge worktree, never a
/// user worktree. Err carries git's stderr.
pub(crate) fn worktree_remove(repo_dir: &Path, path: &Path) -> Result<(), String> {
    let (ok, _, err) = git_in(
        repo_dir,
        &[
            "worktree",
            "remove",
            "--force",
            path.to_str().unwrap_or("/tmp/none"),
        ],
    );
    if ok {
        Ok(())
    } else {
        Err(err)
    }
}

/// List registered worktrees (`git worktree list --porcelain`), from the
/// repository. Err carries git's stderr.
pub(crate) fn worktree_list(repo_dir: &Path) -> Result<String, String> {
    let (ok, out, err) = git_in(repo_dir, &["worktree", "list", "--porcelain"]);
    if ok {
        Ok(out)
    } else {
        Err(err)
    }
}

/// Lock the temp worktree so `git worktree remove --force` fails
/// deterministically. Debug-only test seam (VAL-027): inert in release
/// builds and when the env var is unset. Err carries git's stderr.
#[cfg(debug_assertions)]
pub(crate) fn worktree_lock(repo_dir: &Path, path: &Path) -> Result<(), String> {
    let (ok, _, err) = git_in(
        repo_dir,
        &["worktree", "lock", path.to_str().unwrap_or("/tmp/none")],
    );
    if ok {
        Ok(())
    } else {
        Err(err)
    }
}

/// True if `base_ref` is checked out in a non-temporary worktree (main or a
/// linked one). Refuses advancing a checked-out branch without updating its
/// working tree (wire contract § Checked-out base guard). Aborts (Err) if the
/// worktree list cannot be enumerated — a failed guard must not silently
/// bypass the check.
pub(crate) fn base_checked_out_elsewhere(repo_dir: &Path, base_ref: &str) -> Result<bool, String> {
    use std::process::Command;
    let out = Command::new("git")
        .arg("-C")
        .arg(repo_dir)
        .args(["worktree", "list", "--porcelain"])
        .output()
        .map_err(|e| format!("git worktree list failed: {e}"))?;
    if !out.status.success() {
        return Err("git worktree list failed".into());
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let want = format!("branch refs/heads/{base_ref}");
    Ok(text.lines().any(|l| l.trim() == want))
}

/// Execute the requested merge strategy inside the temp worktree and return
/// the resulting HEAD oid (as a string). On any failure the in-flight
/// merge/rebase state is cleaned up (abort/reset + `git clean -fd`) and the
/// temp worktree is removed; the returned Err is the full user-facing error.
///
/// Squash ordering (VAL-101, pre-extraction behavior): `git merge --squash`
/// runs first (staging), THEN the title is checked — a missing/empty title
/// produces the "PR has no title" cleanup error AFTER staging. `title` is
/// only used by the squash path; rebase/merge ignore it.
pub(crate) fn execute_strategy(
    repo_dir: &Path,
    tmp: &Path,
    kind: &str,
    source_oid: git2::Oid,
    base_oid: git2::Oid,
    merge_base: git2::Oid,
    title: &str,
) -> Result<String, String> {
    match kind {
        "rebase" => {
            let (ok, _o, e) = git_in(
                tmp,
                &[
                    "rebase",
                    "--onto",
                    &base_oid.to_string(),
                    &merge_base.to_string(),
                ],
            );
            if !ok {
                return Err(cleanup_failed_worktree(repo_dir, tmp, "rebase", e));
            }
        }
        "squash" => {
            // `git merge --squash` runs first (staging); only then is the
            // title checked, so a missing/empty title hits the cleanup path
            // AFTER staging (reset --hard HEAD + clean + worktree removal) —
            // exactly the pre-extraction ordering (VAL-101).
            let (ok, _o, e) = git_in(tmp, &["merge", "--squash", &source_oid.to_string()]);
            if !ok {
                return Err(cleanup_failed_worktree(repo_dir, tmp, "squash", e));
            }
            if title.is_empty() {
                return Err(cleanup_failed_worktree(
                    repo_dir,
                    tmp,
                    "squash",
                    "PR has no title".to_string(),
                ));
            }
            let (ok2, _o2, e2) = git_in(tmp, &["commit", "-m", title]);
            if !ok2 {
                return Err(cleanup_failed_worktree(repo_dir, tmp, "squash", e2));
            }
        }
        _ => {
            // default merge commit; --no-ff --no-edit, never opens an editor.
            let (ok, _o, e) = git_in(
                tmp,
                &["merge", "--no-ff", "--no-edit", &source_oid.to_string()],
            );
            if !ok {
                return Err(cleanup_failed_worktree(repo_dir, tmp, "merge", e));
            }
        }
    }
    let (ok2, out2, e2) = git_in(tmp, &["rev-parse", "HEAD"]);
    if !ok2 {
        return Err(cleanup_failed_worktree(repo_dir, tmp, kind, e2));
    }
    Ok(out2)
}

/// Clean up a failed strategy execution: abort merge/rebase (rebase only when
/// state exists), run `git clean -fd`, remove the temp worktree. Returns the
/// user-facing error message.
pub(crate) fn cleanup_failed_worktree(
    repo_dir: &Path,
    tmp: &Path,
    kind: &str,
    cause: String,
) -> String {
    match kind {
        // Squash failure: `git merge --squash` stages without a MERGE_HEAD,
        // so `merge --abort` cannot reset. Reset the detached worktree back to
        // HEAD (= the /base OID it was added at), then clean + remove.
        "squash" => {
            let _ = git_in(tmp, &["reset", "--hard", "HEAD"]);
        }
        // Default merge failure: abort the in-flight merge (MERGE_HEAD state).
        "merge" => {
            let (state_ok, _, _) = git_in(tmp, &["rev-parse", "--verify", "-q", "MERGE_HEAD"]);
            if state_ok {
                let _ = git_in(tmp, &["merge", "--abort"]);
            }
        }
        // Rebase failure: abort only when rebase state exists.
        "rebase" => {
            if rebase_in_progress(tmp) {
                let _ = git_in(tmp, &["rebase", "--abort"]);
            }
        }
        _ => {}
    }
    let _ = git_in(tmp, &["clean", "-fd"]);
    match worktree_remove(repo_dir, tmp) {
        Ok(()) => {}
        Err(err_rm) => {
            // The registration is still live (e.g. the worktree is locked), so
            // deleting the directory underneath it would leave git's registration
            // pointing at a path that no longer exists — a dangling worktree
            // (AC-005d: no registration without its directory). Preserve the
            // leftover and report it so the operator can unlock/remove it.
            // Release the sibling lock so the suffix stays reusable.
            let _ = std::fs::remove_file(tmp.with_extension("lock"));
            return format!(
                "git {kind} failed: {cause}; temp worktree removal failed and the \
                 worktree is left at \"{}\" ({err_rm}) — unlock and remove it manually",
                tmp.display()
            );
        }
    }
    // `git worktree remove` already removed the directory; do NOT run a
    // recursive delete here (each path is owned by whichever merge holds its
    // sibling lock — deleting another process's worktree is never safe).
    // AC-005h: verify the disposable directory is actually gone.
    let _ = std::fs::remove_file(tmp.with_extension("lock"));
    format!("git {kind} failed: {cause}; worktree cleaned up, no ref changes made")
}

/// `git merge-base --all <a> <b>` count must be exactly 1. Zero covers
/// unrelated/shallow histories (shallow → ask to deepen); multiple covers
/// criss-cross. Returns the single merge-base OID.
pub(crate) fn require_single_merge_base(
    repo: &git2::Repository,
    a: git2::Oid,
    b: git2::Oid,
) -> Result<git2::Oid, String> {
    use std::process::Command;
    let out = Command::new("git")
        .arg("-C")
        .arg(repo.path())
        .args(["merge-base", "--all", &a.to_string(), &b.to_string()])
        .output()
        .map_err(|e| format!("git merge-base failed: {e}"))?;
    let stdout =
        std::str::from_utf8(&out.stdout).map_err(|_| "merge-base output not utf8".to_string())?;
    let lines: Vec<&str> = stdout.trim().lines().filter(|l| !l.is_empty()).collect();
    // `git merge-base --all` exits 1 with EMPTY stdout when the two commits
    // share no common ancestor (unrelated histories, or shallow/incomplete
    // history). Exit 128 (nonempty stderr) is a genuine git failure. Without
    // this classification the deepen/unshallow guidance below is dead code.
    if !out.status.success() {
        if out.status.code() == Some(1) && lines.is_empty() {
            return Err(zero_merge_base_error(repo));
        }
        return Err(format!(
            "git merge-base failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    if lines.len() != 1 {
        if lines.is_empty() {
            return Err(zero_merge_base_error(repo));
        }
        return Err(format!(
            "multiple merge bases ({}) suggest a criss-cross history; cannot create the PR",
            lines.len()
        ));
    }
    lines[0]
        .trim()
        .parse::<git2::Oid>()
        .map_err(|_| "invalid merge-base oid".to_string())
}

/// User-facing error for zero merge bases: unrelated histories (no common
/// ancestor) vs shallow/incomplete history (must deepen/unshallow).
fn zero_merge_base_error(repo: &git2::Repository) -> String {
    if repo.is_shallow() {
        "no merge base (shallow history) — deepen/unshallow the repository first".to_string()
    } else {
        "no merge base — the source/base histories are unrelated, or history is shallow; deepen/unshallow before creating a PR".to_string()
    }
}
