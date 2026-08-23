//! CI plan execution: select, materialize and run the immutable-snapshot CI
//! plan against the PR source, under a bounded deadline with process-group
//! termination. Split out of `git.rs` so the general git adapter stays within
//! the size gate while the security-sensitive CI runner is isolated.

use std::path::{Path, PathBuf};
use std::time::Duration;

/// The immutable-snapshot CI plan to execute (F-001/F-012). `label` is the
/// persisted plan name (`.forge/ci.sh` or `just check`); `justfile` is the
/// snapshot justfile path (relative to the worktree root) for `just --justfile`
/// when the label is the fallback.
pub(crate) struct CiPlan {
    pub(crate) label: &'static str,
    pub(crate) justfile: Option<PathBuf>,
}

/// Outcome of a bounded CI plan run (F-002/F-015): the exit status (None when
/// the process could not be spawned, the snapshot plan could not be
/// materialized, or the plan exceeded the bounded deadline) and whether the
/// deadline was hit (so a killed group is distinguishable from a spawn
/// failure).
pub(crate) struct CiRun {
    pub(crate) status: Option<i32>,
    pub(crate) timed_out: bool,
}

/// Bound and validate the configured CI deadline (F-011): an extreme
/// `GIT_FORGE_CI_TIMEOUT` (e.g. `u64::MAX`) must not overflow the deadline
/// computation and panic after the child/worktree are created, which would
/// skip the kill/reap, the failed-Check append and the cleanup. An
/// unparseable/absent value falls back to 300s; the value is clamped to
/// [1, 86400] seconds.
pub(crate) fn ci_timeout(env_value: Option<&str>) -> Duration {
    let secs = env_value
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(300)
        .clamp(1, 86_400);
    Duration::from_secs(secs)
}

/// Select the immutable-snapshot CI plan (F-001/F-012). `.forge/ci.sh` when it
/// is a regular file in the snapshot, the pinned `just check` fallback when it
/// is absent, and a refusal when `.forge/ci.sh` (or the fallback justfile) is a
/// symlink or otherwise not a regular file — so CI never follows a tracked link
/// to mutable bytes outside the snapshot. The fallback refuses when the
/// snapshot carries no justfile, so an ancestor/global justfile cannot supply a
/// green Check (F-012).
pub(crate) fn snapshot_ci_plan(repo: &git2::Repository, oid: git2::Oid) -> Result<CiPlan, String> {
    let tree = repo
        .find_commit(oid)
        .and_then(|c| c.tree())
        .map_err(|_| "cannot read PR snapshot source tree".to_string())?;
    match tree.get_path(Path::new(".forge/ci.sh")) {
        Err(_) => {
            for name in ["justfile", "Justfile", ".justfile"] {
                match tree.get_path(Path::new(name)) {
                    Err(_) => continue,
                    Ok(e) => match e.filemode() {
                        0o100644 | 0o100755 | 0o100664 => {
                            return Ok(CiPlan {
                                label: "just check",
                                justfile: Some(PathBuf::from(name)),
                            });
                        }
                        0o120000 => {
                            return Err(format!("refusing {name}: symlink (F-012)"));
                        }
                        _ => {
                            return Err(format!("refusing {name}: not a regular file (F-012)"));
                        }
                    },
                }
            }
            Err("no justfile in PR snapshot for the `just check` fallback (F-012)".to_string())
        }
        Ok(e) => match e.filemode() {
            0o100644 | 0o100755 | 0o100664 => Ok(CiPlan {
                label: ".forge/ci.sh",
                justfile: None,
            }),
            0o120000 => Err("refusing .forge/ci.sh: symlink (F-001)".to_string()),
            _ => Err("refusing .forge/ci.sh: not a regular file (F-001)".to_string()),
        },
    }
}

/// Materialize the immutable snapshot's plan bytes into the worktree so the
/// executed plan is exactly the blob content (F-013): a checkout smudge or
/// attribute filter that rewrote the file (e.g. a `.gitattributes` filter that
/// turns `exit 1` into `exit 0`) can no longer replace the plan that runs.
fn materialize_plan(
    repo: &git2::Repository,
    oid: git2::Oid,
    worktree: &Path,
    plan: &CiPlan,
) -> Result<(), String> {
    let rel = match plan.label {
        ".forge/ci.sh" => ".forge/ci.sh",
        "just check" => plan
            .justfile
            .as_ref()
            .map(|p| p.to_str().unwrap_or("justfile"))
            .unwrap_or("justfile"),
        other => return Err(format!("unknown CI plan '{other}'")),
    };
    let commit = repo
        .find_commit(oid)
        .map_err(|_| "cannot read PR snapshot source tree".to_string())?;
    let tree = commit
        .tree()
        .map_err(|_| "cannot read PR snapshot source tree".to_string())?;
    let entry = tree
        .get_path(Path::new(rel))
        .map_err(|_| format!("plan file '{rel}' missing from snapshot"))?;
    let blob = repo
        .find_blob(entry.id())
        .map_err(|_| format!("cannot read plan blob '{rel}'"))?;
    let dest = worktree.join(rel);
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("cannot create plan directory: {e}"))?;
    }
    // F-012: for the `just check` fallback, pin the Just plan source closure.
    // `set fallback` (or an `import`/`mod` to an absolute/tilde/escaping path)
    // would let a snapshot justfile delegate `check` to mutable ancestor or
    // external bytes, so those are refused before the file is materialized.
    if plan.label == "just check" {
        validate_justfile_closure(blob.content(), rel)?;
    }
    std::fs::write(&dest, blob.content()).map_err(|e| format!("cannot write plan '{rel}': {e}"))?;
    Ok(())
}

/// Reject a `just` source that can escape the immutable snapshot (F-012): a
/// `set fallback` makes `just` delegate a missing recipe to a mutable ancestor
/// justfile, and an `import`/`mod` with an absolute, `~`, or `../`-escaping
/// path reads bytes outside the pinned tree. Relative in-snapshot imports are
/// allowed. Best-effort line scan (ignores `#` comments) — a substantive
/// limitation is documented at the call site.
fn validate_justfile_closure(content: &[u8], rel: &str) -> Result<(), String> {
    let text = String::from_utf8_lossy(content);
    for line in text.lines() {
        let t = line.trim();
        let t = t.split('#').next().unwrap_or("").trim();
        if t.is_empty() {
            continue;
        }
        if t == "set fallback" || t.starts_with("set fallback") {
            return Err(format!(
                "refusing {rel}: `set fallback` would delegate to a mutable ancestor justfile (F-012)"
            ));
        }
        for kw in ["import", "mod"] {
            if let Some(rest) = t.strip_prefix(kw) {
                let path = rest.trim().trim_matches(|c| c == '\'' || c == '"');
                if path.starts_with('/')
                    || path.starts_with('~')
                    || path.starts_with("../")
                    || path.starts_with("./../")
                    || path.contains("..")
                {
                    return Err(format!(
                        "refusing {rel}: `{kw}` source '{path}' escapes the snapshot (F-012)"
                    ));
                }
            }
        }
    }
    Ok(())
}

/// Signal the process group `pgid` (a child that leads its own group) with
/// `sig` via a TRUSTED absolute `/bin/kill` (F-024): a PATH shim must not be
/// able to no-op termination, and `--` is passed so negative-pgid parsing is
/// portable. Returns an error carrying stderr on failure.
fn signal_pg(pgid: i32, sig: &str) -> Result<(), String> {
    let out = std::process::Command::new("/bin/kill")
        .arg("-s")
        .arg(sig)
        .arg("--")
        .arg(format!("-{pgid}"))
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .output();
    match out {
        Ok(o) if o.status.success() => Ok(()),
        Ok(o) => Err(String::from_utf8_lossy(&o.stderr).trim().to_string()),
        Err(e) => Err(format!("failed to spawn /bin/kill: {e}")),
    }
}

/// True if any process in the group `pgid` is still alive (kill(-pgid, 0)
/// probe via trusted `/bin/kill`). Used by F-015 to prove the process group is
/// empty before a leader-exit is treated as a completed run.
fn pgid_alive(pgid: i32) -> bool {
    std::process::Command::new("/bin/kill")
        .arg("-0")
        .arg("--")
        .arg(format!("-{pgid}"))
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|st| st.success())
        .unwrap_or(false)
}

/// Run the repo CI plan inside the temp worktree (the PR's own commits).
///
///
/// F-013 first materializes the exact immutable blob bytes into the worktree,
/// so a smudge/attribute filter cannot replace the plan. The plan's
/// stdout/stderr are redirected to the null device (never buffered in memory),
/// so a high-volume plan cannot grow git-forge's heap. The wait is bounded
/// (`timeout`); on expiry (F-015) the WHOLE process group is killed and reaped
/// (not just the direct child), so a background descendant whose leader exits
/// cannot survive the deadline. Returns the exit status and whether the
/// deadline was hit (F-002/F-015).
pub(crate) fn run_ci_plan(
    repo: &git2::Repository,
    oid: git2::Oid,
    worktree: &Path,
    plan: &CiPlan,
    timeout: Duration,
) -> CiRun {
    use std::os::unix::process::CommandExt as _;
    if materialize_plan(repo, oid, worktree, plan).is_err() {
        // A snapshot plan that cannot be materialized is treated like a spawn
        // failure: record a failed Check and let the cleanup path run.
        return CiRun {
            status: None,
            timed_out: false,
        };
    }
    let (prog, jf_arg): (&str, Option<String>) = match plan.label {
        ".forge/ci.sh" => ("bash", None),
        "just check" => {
            let jf = plan
                .justfile
                .as_ref()
                .map(|p| worktree.join(p))
                .unwrap_or_else(|| worktree.join("justfile"));
            ("just", Some(jf.to_string_lossy().into_owned()))
        }
        _ => ("bash", None),
    };
    let args: Vec<String> = match jf_arg {
        Some(jf) => vec!["--justfile".to_string(), jf, "check".to_string()],
        None => vec![".forge/ci.sh".to_string()],
    };
    let mut child = match std::process::Command::new(prog)
        .args(args.iter().map(|s| s.as_str()))
        .current_dir(worktree)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        // F-027: sanitize the control env so caller env cannot inject or
        // suppress the plan bytes — a `BASH_ENV` that `exit 0`s before a
        // failing script or a `JUST_DRY_RUN=true` that skips a failing recipe
        // must not turn a failing snapshot plan green.
        .env_remove("BASH_ENV")
        .env_remove("ENV")
        .env("JUST_DRY_RUN", "false")
        .process_group(0) // the child leads its own process group (pgid = pid)
        .spawn()
    {
        Ok(c) => c,
        Err(_) => {
            return CiRun {
                status: None,
                timed_out: false,
            };
        }
    };
    let pgid = child.id() as i32;
    // F-026: never fall back to an unchecked `Instant + _` (which can panic near
    // the clock representation maximum after the child/worktree were created);
    // an unrepresentable deadline is treated as an immediate timeout.
    let deadline = match std::time::Instant::now().checked_add(timeout) {
        Some(d) => d,
        None => std::time::Instant::now(),
    };
    while std::time::Instant::now() < deadline {
        match child.try_wait() {
            Ok(Some(st)) => {
                // F-015: treat leader exit as completion only after proving the
                // process group is empty — a `sleep 60 & exit 0` plan reaps its
                // leader but leaves the descendant alive in the group.
                if pgid_alive(pgid) {
                    let _ = signal_pg(pgid, "KILL");
                    let _ = child.wait();
                    return CiRun {
                        status: None,
                        timed_out: true,
                    };
                }
                return CiRun {
                    status: st.code(),
                    timed_out: false,
                };
            }
            Ok(None) => {}
            Err(_) => {
                return CiRun {
                    status: None,
                    timed_out: false,
                };
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    // F-015/F-024: kill+reap the COMPLETE process group via a trusted absolute
    // kill (a PATH shim must not no-op it) so a background descendant cannot
    // survive the bounded deadline; the result is checked so a missing/no-op
    // kill cannot leave the subsequent wait to hang forever.
    let _ = signal_pg(pgid, "KILL");
    let status = child.wait().ok().and_then(|st| st.code());
    CiRun {
        status,
        timed_out: true,
    }
}
