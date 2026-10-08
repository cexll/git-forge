//! Strategy execution and failure classification before worktree cleanup.
use super::{cleanup_failed_worktree, git_in};
use std::path::Path;
/// Strategy failure classification captured before cleanup removes the index.
pub(crate) struct StrategyFailure {
    pub(crate) message: String,
    pub(crate) conflict: bool,
}

impl StrategyFailure {
    fn application(repo_dir: &Path, tmp: &Path, kind: &str, cause: String) -> Self {
        let conflict = git2::Repository::open(tmp)
            .and_then(|repo| repo.index())
            .map(|index| index.has_conflicts())
            .unwrap_or(false);
        Self {
            message: cleanup_failed_worktree(repo_dir, tmp, kind, cause),
            conflict,
        }
    }

    fn operational(repo_dir: &Path, tmp: &Path, kind: &str, cause: String) -> Self {
        Self {
            message: cleanup_failed_worktree(repo_dir, tmp, kind, cause),
            conflict: false,
        }
    }
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
) -> Result<String, StrategyFailure> {
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
                return Err(StrategyFailure::application(repo_dir, tmp, "rebase", e));
            }
        }
        "ff" => {
            // Fast-forward: the caller has already checked merge_base ==
            // base_head; `merge --ff-only` fails closed if git still refuses.
            let (ok, _o, e) = git_in(tmp, &["merge", "--ff-only", &source_oid.to_string()]);
            if !ok {
                return Err(StrategyFailure::operational(repo_dir, tmp, "ff", e));
            }
        }
        "squash" => {
            // `git merge --squash` runs first (staging); only then is the
            // title checked, so a missing/empty title hits the cleanup path
            // AFTER staging (reset --hard HEAD + clean + worktree removal) —
            // exactly the pre-extraction ordering (VAL-101).
            let (ok, _o, e) = git_in(tmp, &["merge", "--squash", &source_oid.to_string()]);
            if !ok {
                return Err(StrategyFailure::application(repo_dir, tmp, "squash", e));
            }
            if title.is_empty() {
                return Err(StrategyFailure::operational(
                    repo_dir,
                    tmp,
                    "squash",
                    "PR has no title".to_string(),
                ));
            }
            let (ok2, _o2, e2) = git_in(tmp, &["commit", "-m", title]);
            if !ok2 {
                return Err(StrategyFailure::application(repo_dir, tmp, "squash", e2));
            }
        }
        _ => {
            // default merge commit; --no-ff --no-edit, never opens an editor.
            let (ok, _o, e) = git_in(
                tmp,
                &["merge", "--no-ff", "--no-edit", &source_oid.to_string()],
            );
            if !ok {
                return Err(StrategyFailure::application(repo_dir, tmp, "merge", e));
            }
        }
    }
    let (ok2, out2, e2) = git_in(tmp, &["rev-parse", "HEAD"]);
    if !ok2 {
        return Err(StrategyFailure::operational(repo_dir, tmp, kind, e2));
    }
    Ok(out2)
}
