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

/// Result of running `git`: exit status (None = could not spawn), stdout,
/// stderr, and — on spawn failure only — the raw io error so callers can
/// format it per their baseline error contract. `stderr` is the raw
/// (trimmed) stderr for nonzero exits; on spawn failure it carries the
/// synthesized `failed to spawn git: …` message (consumed by `git_in`
/// callers), while `raw_spawn_error` holds the underlying error unformatted.
/// `git_in_with_status` is the primary `git` Command constructor; the
/// exceptions are `store.rs`'s `run_update_ref_stdin` and `cli.rs`'s
/// `cmd_pr_diff`.
struct GitResult {
    status: Option<i32>,
    stdout: String,
    stderr: String,
    raw_spawn_error: Option<std::io::Error>,
}

/// Run `git` in the given working directory, returning the exit status,
/// stdout, stderr and raw spawn error. This is the primary `git` Command
/// constructor; the exceptions are `store.rs`'s `run_update_ref_stdin` and
/// `cli.rs`'s `cmd_pr_diff`.
fn git_in_with_status(dir: &Path, args: &[&str]) -> GitResult {
    use std::process::Command;
    let out = Command::new("git").arg("-C").arg(dir).args(args).output();
    match out {
        Ok(o) => GitResult {
            status: o.status.code(),
            stdout: String::from_utf8_lossy(&o.stdout).trim().to_string(),
            stderr: String::from_utf8_lossy(&o.stderr).trim().to_string(),
            raw_spawn_error: None,
        },
        Err(e) => GitResult {
            status: None,
            stdout: String::new(),
            stderr: format!("failed to spawn git: {e}"),
            raw_spawn_error: Some(e),
        },
    }
}

/// Run `git` in the given working directory, returning (success, stdout,
/// stderr). Used for worktree merge/rebase execution and cleanup.
fn git_in(dir: &Path, args: &[&str]) -> (bool, String, String) {
    let r = git_in_with_status(dir, args);
    (r.status == Some(0), r.stdout, r.stderr)
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

/// Add the disposable CI temp worktree without running repository-relative
/// hooks (F-014): a post-checkout hook that a snapshot's environment could
/// supply must not run during the CI checkout and hang `ci run` or accumulate
/// output before its deadline. `core.hooksPath=/dev/null` redirects hook lookup
/// to a path that is never a directory, so git finds no hooks.
pub(crate) fn worktree_add_ci(
    repo_dir: &Path,
    path: &Path,
    detach_oid: &git2::Oid,
) -> Result<(), String> {
    let (ok, _, err) = git_in(
        repo_dir,
        &[
            "-c",
            "core.hooksPath=/dev/null",
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

/// Raw `git worktree list --porcelain` executor, `git_in`-compatible:
/// returns (status-ok, stdout, stderr) with stderr as the raw (trimmed)
/// stderr for nonzero exits and the synthesized `failed to spawn git: …`
/// on spawn failure. Used by `cmd_pr_merge`'s post-removal verification,
/// which surfaces the raw diagnostic (baseline 6904cd3 behavior). Runs
/// from the repository, never from a worktree's parent.
pub(crate) fn worktree_list_raw(repo_dir: &Path) -> (bool, String, String) {
    git_in(repo_dir, &["worktree", "list", "--porcelain"])
}

/// List registered worktrees (`git worktree list --porcelain`), from the
/// repository. Thin base-guard wrapper over `worktree_list_raw` mapping to
/// the bare baseline messages: spawn failure -> `git worktree list
/// failed: {raw io error}`; nonzero git exit -> exactly `git worktree
/// list failed` (stderr never appended).
pub(crate) fn worktree_list(repo_dir: &Path) -> Result<String, String> {
    let r = git_in_with_status(repo_dir, &["worktree", "list", "--porcelain"]);
    match r.status {
        Some(0) => Ok(r.stdout),
        None => match r.raw_spawn_error {
            Some(e) => Err(format!("git worktree list failed: {e}")),
            None => Err("git worktree list failed".to_string()),
        },
        Some(_) => Err("git worktree list failed".to_string()),
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
    let text = worktree_list(repo_dir)?;
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
    let r = git_in_with_status(
        repo.path(),
        &["merge-base", "--all", &a.to_string(), &b.to_string()],
    );
    let lines: Vec<&str> = r.stdout.lines().filter(|l| !l.is_empty()).collect();
    // `git merge-base --all` exits 1 with EMPTY stdout when the two commits
    // share no common ancestor (unrelated histories, or shallow/incomplete
    // history). Any other failure exit (e.g. 128) is a genuine git failure,
    // regardless of stderr content — classification is by EXIT STATUS, not
    // stderr heuristics. Spawn failure formats the raw io error (baseline);
    // nonzero exits append the raw (unprefixed) stderr.
    if r.status != Some(0) {
        match r.status {
            None => {
                return Err(match r.raw_spawn_error {
                    Some(e) => format!("git merge-base failed: {e}"),
                    None => "git merge-base failed".to_string(),
                });
            }
            Some(1) if lines.is_empty() => return Err(zero_merge_base_error(repo)),
            Some(_) => return Err(format!("git merge-base failed: {}", r.stderr.trim())),
        }
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

/// A single config value read from the git binary (byte-exact). `Some` means
/// a value was present and decodes as UTF-8 (empty/whitespace values are
/// Some("") / Some("   ") — the caller applies the trim/fallback policy);
/// `None` means the key is absent, the value is empty/whitespace, or the
/// value is non-UTF-8 (all current fallback cases, F-028). An `Err` is a real
/// config-open parse failure / spawn failure — STAGE A, propagates.
///
/// Command: `git -C <worktree> config --null --get user.email`.
/// Contract (verified bytes, git 2.x): present -> exit 0 + `value\0`;
/// missing -> exit 1 + empty stdout; empty value -> exit 0 + `\0`;
/// whitespace -> exit 0 + `   \0`; raw non-UTF-8 bytes -> exit 0 + raw bytes
/// (passed through verbatim, so UTF-8-decode failure below = fallback case);
/// malformed .git/config -> exit 128. Because absence is exit 1 and an empty
/// value is exit 0 + `\0`, the classification uses status AND stdout shape.
fn config_get_string(worktree: &Path, key: &str) -> Result<Option<String>, String> {
    use std::process::Command;
    let out = Command::new("git")
        .arg("-C")
        .arg(worktree)
        .args(["config", "--null", "--get", key])
        .output()
        .map_err(|e| format!("failed to spawn git config: {e}"))?;
    classify_config_get(out.status.code(), &out.stdout, &out.stderr, key)
}

/// Strip terminal control characters from a git config diagnostic before it
/// is embedded in a CLI error and printed, because git's stderr can echo a
/// repo config value (possibly attacker-controlled) that contains an ansi
/// escape; escaping C0, DEL, and the C1 block keeps the error text but
/// prevents a terminal action (U+009B is CSI on a UTF-8 virtual console).
fn sanitize_config_diag(bytes: &[u8]) -> String {
    let s = String::from_utf8_lossy(bytes);
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        let cp = c as u32;
        if cp < 0x20 || (0x7F..=0x9F).contains(&cp) {
            out.push_str(&format!("\\x{:02x}", cp));
        } else {
            out.push(c);
        }
    }
    out
}

/// Classify the raw `git config --null --get <key>` subprocess outcome into
/// the identity value. Pure: takes the exit status, raw stdout/stderr bytes
/// and key, and returns the parsed value or a Stage-A error. Split out of
/// `config_get_string` so every branch is unit-testable without spawning git.
///
/// Branch contract:
/// - `Some(128)` / `None` (spawn failure surfaces as no status) -> Err
///   (unreadable config). Never silently downgraded to a fallback.
/// - `Some(0)` -> stdout must be exactly `value\0` (one trailing NUL, no
///   interior NUL); anything else -> Err (malformed output). `value` that is
///   not UTF-8 -> `Ok(None)` (F-028 non-UTF-8 fallback).
/// - `Some(1)` with EMPTY stdout AND stderr -> `Ok(None)` (absent key). Any
///   diagnostic on exit 1 is a real config error -> Err.
/// - any other status -> Err with the diagnostic.
fn classify_config_get(
    status: Option<i32>,
    stdout: &[u8],
    stderr: &[u8],
    key: &str,
) -> Result<Option<String>, String> {
    match status {
        Some(128) | None => Err(format!(
            "repo config is unreadable (git config --get {key} failed): {}",
            sanitize_config_diag(stderr).trim()
        )),
        Some(0) => {
            // stdout must be exactly `value\0`: one trailing NUL, no interior
            // NUL, no trailing garbage. Anything else is untrustworthy output
            // from the subprocess — Stage-A error, never a silent fallback.
            let raw = match stdout.strip_suffix(b"\0") {
                Some(body) if !body.contains(&0) => body,
                _ => {
                    return Err(format!(
                        "git config --get {key} returned malformed output on exit 0"
                    ))
                }
            };
            // Non-UTF-8 raw bytes (F-028) decode-fail -> None (fallback).
            match std::str::from_utf8(raw) {
                Ok(s) => Ok(Some(s.to_string())),
                Err(_) => Ok(None),
            }
        }
        Some(other) => {
            // Absent key is exit 1 with BOTH empty stdout and empty stderr.
            // Any diagnostic output on exit 1 is a real config error and must
            // not be silently downgraded to the absent-value fallback.
            if other == 1 && stdout.is_empty() && stderr.is_empty() {
                return Ok(None);
            }
            Err(format!(
                "git config --get {key} exited {other}: {}",
                sanitize_config_diag(stderr).trim()
            ))
        }
    }
}

/// Resolve the repo's configured identity from the git binary, byte-exact.
/// `name`/`email` are `Some` only when present AND UTF-8; empty/whitespace/
/// absent/non-UTF-8 values come back as `None` (caller applies the email-only
/// vs signature-default precedence, matching `repo.signature()` semantics —
/// git-forge only uses a configured committer identity when BOTH name and
/// email are usable). `Err` is a config-open failure (STAGE A): malformed
/// `.git/config`, spawn failure.
pub(crate) fn config_get_identity(
    worktree: &Path,
) -> Result<(Option<String>, Option<String>), String> {
    let email = config_get_string(worktree, "user.email")?;
    let name = config_get_string(worktree, "user.name")?;
    // Trim: empty/whitespace values are not usable identities.
    let email = email.filter(|v| !v.trim().is_empty());
    let name = name.filter(|v| !v.trim().is_empty());
    Ok((name, email))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::process::Command;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(0);

    /// Fresh exclusive temp dir (never reused). create_dir_all on a unique
    /// per-process-per-test path; parent /tmp is not a git repo so the
    /// "not a git repository" branches below are stable.
    fn temp_dir() -> PathBuf {
        let seq = NEXT.fetch_add(1, Ordering::Relaxed);
        let d = std::env::temp_dir().join(format!("gf-gitcfg-{}-{seq}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// A git repo with a valid empty-ish identity, ready for local config edits.
    fn init_repo() -> PathBuf {
        let d = temp_dir();
        let st = Command::new("git")
            .args(["init", "-q"])
            .current_dir(&d)
            .status()
            .unwrap();
        assert!(st.success(), "git init failed");
        d
    }

    /// Overwrite `.git/config` with raw bytes (supports non-UTF-8 / malformed
    /// content that `git config` itself could not write).
    fn write_config(d: &Path, bytes: &[u8]) {
        std::fs::create_dir_all(d.join(".git")).unwrap();
        std::fs::write(d.join(".git").join("config"), bytes).unwrap();
    }

    // ── classify_config_get: every branch, pure (no git spawn) ──

    #[test]
    fn classify_128_and_spawn_failure_are_unreadable() {
        let err = classify_config_get(Some(128), &[], b"fatal: bad config line 1", "user.email")
            .unwrap_err();
        assert!(err.contains("repo config is unreadable"), "{err}");
        // status None models a spawn failure (no status observed).
        let err2 = classify_config_get(None, &[], b"", "user.email").unwrap_err();
        assert!(err2.contains("repo config is unreadable"), "{err2}");
    }

    #[test]
    fn classify_exit0_shape_variants() {
        // present value
        assert_eq!(
            classify_config_get(Some(0), b"dev@example.com\0", b"", "user.email").unwrap(),
            Some("dev@example.com".to_string())
        );
        // empty value -> exit 0 + bare NUL
        assert_eq!(
            classify_config_get(Some(0), b"\0", b"", "user.email").unwrap(),
            Some(String::new())
        );
        // whitespace value
        assert_eq!(
            classify_config_get(Some(0), b"   \0", b"", "user.email").unwrap(),
            Some("   ".to_string())
        );
        // non-UTF-8 raw bytes -> None (F-028 fallback), never an error
        assert_eq!(
            classify_config_get(Some(0), b"\xff\xfe\0", b"", "user.email").unwrap(),
            None
        );
    }

    #[test]
    fn classify_exit0_malformed_output_is_error() {
        // interior NUL -> untrustworthy -> Err, never a fallback
        let e = classify_config_get(Some(0), b"a\0b\0", b"", "user.email").unwrap_err();
        assert!(e.contains("malformed output"), "{e}");
        // missing trailing NUL -> Err
        let e2 = classify_config_get(Some(0), b"no-trailing-nul", b"", "user.email").unwrap_err();
        assert!(e2.contains("malformed output"), "{e2}");
    }

    #[test]
    fn classify_absent_is_exit1_empty_both() {
        // absent key: exit 1, both streams empty -> Ok(None)
        assert_eq!(
            classify_config_get(Some(1), &[], &[], "user.email").unwrap(),
            None
        );
        // exit 1 WITH a diagnostic is a real config error, not absence
        let e = classify_config_get(Some(1), &[], b"fatal: not a git repository", "user.email")
            .unwrap_err();
        assert!(e.contains("exited 1"), "{e}");
        // other nonzero status
        let e2 = classify_config_get(Some(5), &[], b"boom", "user.email").unwrap_err();
        assert!(e2.contains("exited 5"), "{e2}");
    }

    // ── config_get_identity: real-git smoke, local-config-authored (stable) ──

    #[test]
    fn identity_uses_both_when_present() {
        let d = init_repo();
        let st = Command::new("git")
            .args(["config", "user.email", "dev@example.com"])
            .current_dir(&d)
            .status()
            .unwrap();
        assert!(st.success());
        let st = Command::new("git")
            .args(["config", "user.name", "Dev"])
            .current_dir(&d)
            .status()
            .unwrap();
        assert!(st.success());
        let (name, email) = config_get_identity(&d).unwrap();
        assert_eq!(name.as_deref(), Some("Dev"));
        assert_eq!(email.as_deref(), Some("dev@example.com"));
    }

    #[test]
    fn identity_filters_empty_email_but_keeps_name() {
        let d = init_repo();
        let st = Command::new("git")
            .args(["config", "--local", "user.email", ""])
            .current_dir(&d)
            .status()
            .unwrap();
        assert!(st.success());
        let st = Command::new("git")
            .args(["config", "--local", "user.name", "Dev"])
            .current_dir(&d)
            .status()
            .unwrap();
        assert!(st.success());
        let (name, email) = config_get_identity(&d).unwrap();
        assert_eq!(name.as_deref(), Some("Dev"));
        assert_eq!(email, None, "empty email must be filtered to None");
    }

    #[test]
    fn identity_surfaces_malformed_config_as_err() {
        let d = init_repo();
        write_config(&d, b"[user\nemail = broken");
        let e = config_get_identity(&d).unwrap_err();
        assert!(e.contains("repo config is unreadable"), "{e}");
    }
}
