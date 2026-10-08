//! Strategy execution and failure classification before worktree cleanup.
use super::{cleanup_after_failure, git_in};
use std::path::Path;
/// Strategy failure classification captured before cleanup removes the index.
pub(crate) struct StrategyFailure {
    /// `git <kind> failed: <cause>; <cleanup>`: the operational message.
    pub(crate) message: String,
    /// Paths of the unmerged index entries, one per path; empty when
    /// the failure left no unmerged entry (not a conflict).
    pub(crate) conflict_paths: Vec<String>,
    /// The cleanup outcome clause that ends `message`.
    pub(crate) cleanup: String,
}

impl StrategyFailure {
    fn application(repo_dir: &Path, tmp: &Path, kind: &str, cause: String) -> Self {
        let conflict_paths = conflicted_paths(tmp);
        Self::cleaned(repo_dir, tmp, kind, cause, conflict_paths)
    }

    fn operational(repo_dir: &Path, tmp: &Path, kind: &str, cause: String) -> Self {
        Self::cleaned(repo_dir, tmp, kind, cause, Vec::new())
    }

    fn cleaned(
        repo_dir: &Path,
        tmp: &Path,
        kind: &str,
        cause: String,
        conflict_paths: Vec<String>,
    ) -> Self {
        let cleanup = cleanup_after_failure(repo_dir, tmp, kind);
        Self {
            message: format!("git {kind} failed: {cause}; {cleanup}"),
            conflict_paths,
            cleanup,
        }
    }
}

/// Paths of the unmerged entries (stage 1-3) in the worktree's index, one per
/// exact path byte sequence, in index order of first appearance. Raw entries
/// are grouped here rather than through libgit2's conflict iterator, which
/// groups stages with the index's path comparison and so folds `A.txt` and
/// `a.txt` together under `core.ignorecase`. Non-UTF-8 bytes are replaced. An
/// unreadable index yields no paths, so the failure stays operational.
fn conflicted_paths(worktree: &Path) -> Vec<String> {
    /// Stage bits of `git_index_entry.flags` (`GIT_INDEX_ENTRY_STAGEMASK`).
    const STAGE_MASK: u16 = 0x3000;
    let Ok(index) = git2::Repository::open(worktree).and_then(|repo| repo.index()) else {
        return Vec::new();
    };
    let mut seen = std::collections::HashSet::new();
    index
        .iter()
        .filter(|entry| entry.flags & STAGE_MASK != 0)
        .filter(|entry| seen.insert(entry.path.clone()))
        .map(|entry| String::from_utf8_lossy(&entry.path).into_owned())
        .collect()
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

#[cfg(test)]
mod tests {
    use super::conflicted_paths;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    fn git(dir: &Path, args: &[&str]) -> bool {
        Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=Test", "-c", "user.email=test@example.com"])
            .args(args)
            .output()
            .unwrap()
            .status
            .success()
    }

    fn repo(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("gf-conflicted-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert!(git(&dir, &["init", "-q", "-b", "main"]));
        for (path, content) in [("b.txt", "b\n"), ("d.txt", "d\n"), ("clean.txt", "c\n")] {
            std::fs::write(dir.join(path), content).unwrap();
        }
        assert!(git(&dir, &["add", "-A"]));
        assert!(git(&dir, &["commit", "-qm", "ancestor"]));
        dir
    }

    fn commit_side(dir: &Path, branch: &str, edits: &[(&str, Option<&str>)]) {
        assert!(git(dir, &["checkout", "-q", "-B", branch, "main"]));
        for (path, content) in edits {
            match content {
                Some(content) => std::fs::write(dir.join(path), content).unwrap(),
                None => std::fs::remove_file(dir.join(path)).unwrap(),
            }
        }
        assert!(git(dir, &["add", "-A"]));
        assert!(git(dir, &["commit", "-qm", branch]));
    }

    #[test]
    fn conflicted_paths_lists_each_unmerged_path_once() {
        let dir = repo("unmerged");
        let source = [
            ("b.txt", Some("source\n")),
            ("d.txt", None),
            ("a.txt", Some("source\n")),
        ];
        commit_side(&dir, "source", &source);
        let base = [
            ("b.txt", Some("base\n")),
            ("d.txt", Some("base\n")),
            ("a.txt", Some("base\n")),
        ];
        commit_side(&dir, "base", &base);
        assert!(!git(&dir, &["merge", "--no-ff", "--no-edit", "source"]));
        // Content (b), modify/delete (d) and add/add (a) conflicts each
        // appear once although they hold two or three index stages.
        assert_eq!(conflicted_paths(&dir), ["a.txt", "b.txt", "d.txt"]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn conflicted_paths_keeps_paths_that_differ_only_in_case() {
        let dir = repo("ignorecase");
        assert!(git(&dir, &["config", "core.ignorecase", "true"]));
        let records: String = ["A.txt", "a.txt"]
            .iter()
            .flat_map(|path| {
                (1..=3).map(move |stage| {
                    format!("100644 e69de29bb2d1d6434b8b29ae775ad8c2e48c5391 {stage}\t{path}\0")
                })
            })
            .collect();
        let mut child = Command::new("git")
            .arg("-C")
            .arg(&dir)
            .args(["update-index", "-z", "--index-info"])
            .stdin(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        use std::io::Write;
        child
            .stdin
            .take()
            .unwrap()
            .write_all(records.as_bytes())
            .unwrap();
        assert!(child.wait().unwrap().success());
        assert_eq!(conflicted_paths(&dir), ["A.txt", "a.txt"]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn conflicted_paths_is_empty_without_unmerged_entries() {
        let dir = repo("clean");
        assert!(conflicted_paths(&dir).is_empty());
        assert!(conflicted_paths(&dir.join("missing")).is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
