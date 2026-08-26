//! CI plan execution: select, materialize and run the immutable-snapshot CI
//! plan against the PR source, under a bounded deadline with process-group
//! termination. Split out of `git.rs` so the general git adapter stays within
//! the size gate while the security-sensitive CI runner is isolated.

use std::path::Path;
use std::time::Duration;

/// The immutable-snapshot CI plan to execute (F-001/F-012). The variant is
/// the persisted plan name (`.forge/ci.sh` or `just check`); `JustCheck`
/// carries the snapshot justfile path relative to the worktree root
/// (`justfile` / `Justfile` / `.justfile`) for `just --justfile`.
pub(crate) enum CiPlan {
    /// `.forge/ci.sh` from the immutable snapshot.
    CiSh,
    /// The `just check` fallback over a snapshot justfile (its relative name:
    /// `justfile` / `Justfile` / `.justfile`).
    JustCheck(&'static str),
}

impl CiPlan {
    /// The persisted plan label (`.forge/ci.sh` or `just check`) recorded in the
    /// ci.check event body and shown in CLI output (F-001/F-012).
    pub(crate) fn label(&self) -> &'static str {
        match self {
            CiPlan::CiSh => ".forge/ci.sh",
            CiPlan::JustCheck(_) => "just check",
        }
    }
}

/// Outcome of a bounded CI plan run (F-002/F-015): the exit status (None when
/// the process could not be spawned, the snapshot plan could not be
/// materialized, or the plan exceeded the bounded deadline) and whether the
/// deadline was hit (so a killed group is distinguishable from a spawn
/// failure).
pub(crate) struct CiRun {
    pub(crate) status: Option<i32>,
    pub(crate) timed_out: bool,
    /// Why a non-green run is not green when it is neither a plain non-zero exit
    /// nor a deadline timeout (F-015): a surviving background descendant or an
    /// unprovable group probe. Drives the user-facing reason so a
    /// descendant-survival failure is not mislabeled "timed out".
    pub(crate) detail: Option<&'static str>,
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
                            return Ok(CiPlan::JustCheck(name));
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
            0o100644 | 0o100755 | 0o100664 => Ok(CiPlan::CiSh),
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
    let rel: &str = match plan {
        CiPlan::CiSh => ".forge/ci.sh",
        CiPlan::JustCheck(name) => name,
    };
    let commit = repo
        .find_commit(oid)
        .map_err(|_| "cannot read PR snapshot source tree".to_string())?;
    let entry = commit
        .tree()
        .and_then(|t| t.get_path(Path::new(rel)))
        .map_err(|_| format!("plan file '{rel}' missing from snapshot"))?;
    let blob = repo
        .find_blob(entry.id())
        .map_err(|_| format!("cannot read plan blob '{rel}'"))?;
    let dest = worktree.join(rel);
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("cannot create plan directory: {e}"))?;
    }
    if matches!(plan, CiPlan::JustCheck(_)) {
        // F-012/F-013: a `just check` fallback must be a SELF-CONTAINED
        // justfile — `set fallback` and any `import`/`mod` source directive are
        // refused below, so the only source `just --justfile` reads is this
        // single immutable blob (never a checkout-backed smudge/rewritten file).
        validate::validate_justfile_closure(blob.content(), rel)?;
    }
    std::fs::write(&dest, blob.content()).map_err(|e| format!("cannot write plan '{rel}': {e}"))?;
    Ok(())
}

/// Reject a `just` source that can escape the immutable snapshot (F-012): a
/// `set fallback` makes `just` delegate a missing recipe to a mutable ancestor
/// justfile, and ANY `import`/`mod` source directive makes the fallback read
/// another just source whose bytes `just` resolves at execution (implicit
/// module candidates, shell-expanded/decoded string paths). This runner cannot
/// reliably pin that closure, so the `just check` fallback must be a
/// SELF-CONTAINED justfile — `set fallback` and `import`/`mod` are refused.
///
/// This is a best-effort line scanner, and it deliberately errs toward
/// REJECTION on ambiguity: a construct it cannot prove immutable (e.g. a literal
/// `set fallback` / `import 'x'` inside a multiline `'''…'''` string, or a
/// directive-shaped line inside an exotic quoting construct) is refused, which
/// records a FAILED Check — never a false green. Rejecting a valid-but-exotic
/// self-contained justfile is the safe direction for a security validator whose
/// invariant is "the executed plan is exactly the immutable blob".
/// True if a top-level `just` line is a REFUSED `set` directive — `set
/// fallback` (delegates a missing recipe to a mutable ancestor justfile) or a
/// `set dotenv-*` setting (loads MUTABLE EXTERNAL bytes from a dotenv file —
/// e.g. it can re-export `BASH_ENV` into the recipe shell to fake a green).
/// A recipe header (`set x:`) and a disambiguating `... := false` are NOT the
/// enabling setting and are allowed.
/// Index of the first `:` that is not inside a single- or double-quoted string.
/// A recipe header's signature colon (`name:`) is unquoted, while a `:` inside
/// an `import`/`mod`/`set` string argument is quoted — so the header test must
/// not mistake a quoted colon for header punctuation (F-012).
mod validate;

/// Signal the process group `pgid` (a child that leads its own group) with
/// `sig` via a DIRECT `kill(2)` syscall (F-024/F-015): no subprocess means no
/// fork+exec latency window, so the reap-to-signal gap is a single instruction
/// and a reaped leader's pid cannot realistically be recycled into an unrelated
/// group in time (the window is minimized, not eliminated — a documented
/// containment residual). It also removes the `/bin/kill` spawn-failure mode
/// and the locale-fragile stderr parse.
fn signal_pg(pgid: i32, sig: i32) -> Result<(), String> {
    // Safety: `pgid` is the child's process-group id and `sig` a valid signal
    // number; `kill(2)` is always safe to call.
    let rc = unsafe { libc::kill(-pgid, sig) };
    if rc == 0 {
        Ok(())
    } else {
        Err(format!(
            "kill -{pgid} signal {sig} failed: {}",
            std::io::Error::last_os_error()
        ))
    }
}

/// True if any process in the group `pgid` is still alive (a direct
/// `kill(-pgid, 0)` syscall), OR an error if the group's liveness CANNOT be
/// proven (an unexpected errno). Used by F-015 to prove the process group is
/// empty before a leader-exit is treated as a completed run: an unprovable
/// probe must FAIL the run, never assume the group is empty (a descendant
/// could still be alive). The direct syscall keeps the probe-to-KILL window
/// at nanoseconds, so a reaped leader's pid cannot realistically be recycled
/// between the probe and the signal.
fn pgid_alive(pgid: i32) -> Result<bool, String> {
    // Safety: `pgid` is the child's process-group id; signal 0 is a pure
    // liveness probe and `kill(2)` is always safe to call.
    let rc = unsafe { libc::kill(-pgid, 0) };
    if rc == 0 {
        return Ok(true);
    }
    // `kill(2)` returns ESRCH for an empty group and EPERM for an EXISTING
    // but unsignalable group — read errno to distinguish them: only ESRCH
    // proves the group is empty; EPERM/anything else is "cannot prove empty"
    // and fails closed.
    let err = std::io::Error::last_os_error();
    match err.raw_os_error() {
        Some(libc::ESRCH) => Ok(false),
        _ => Err(format!(
            "kill -0 -{pgid} failed (cannot prove the group is empty): {err}"
        )),
    }
}

/// Remove the dynamic-loader injection variables (`LD_*`/`DYLD_*`) from a
/// Command: a caller-supplied constructor can otherwise run before the trusted
/// program and exit 0 or alter its behavior. Shared by the plan interpreter
/// and the CI lifecycle `git` (F-027).
pub(crate) fn sanitize_loader_env(cmd: &mut std::process::Command) {
    for var in [
        "LD_PRELOAD",
        "LD_AUDIT",
        "LD_LIBRARY_PATH",
        "LD_TRACE_LOADED_OBJECTS",
        "LD_DEBUG",
        "DYLD_INSERT_LIBRARIES",
        "DYLD_FORCE_TEXT_SEGMENT",
        "DYLD_LIBRARY_PATH",
        "DYLD_FRAMEWORK_PATH",
        "DYLD_FALLBACK_FRAMEWORK_PATH",
        "DYLD_VERSIONED_FRAMEWORK_PATH",
        "DYLD_FALLBACK_LIBRARY_PATH",
        "DYLD_VERSIONED_LIBRARY_PATH",
        "DYLD_IMAGE_SUFFIX",
        "DYLD_ROOT_PATH",
    ] {
        cmd.env_remove(var);
    }
}

fn tighten_ci_env(cmd: &mut std::process::Command) {
    for var in [
        "BASH_ENV",
        "ENV",
        "ZDOTDIR",
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_COMMON_DIR",
        "GIT_NAMESPACE",
        "GIT_CONFIG",
        "GIT_CONFIG_COUNT",
        "GIT_CONFIG_PARAMETERS",
        "GIT_ATTR_SOURCE",
        "GIT_EXTERNAL_DIFF",
        "GIT_EXTERNAL_DIFF_TRUST_EXIT_CODE",
        // A plan `git` command's pass/fail output must come from the immutable
        // snapshot, never a caller-redirected replace/shallow/ceiling object
        // store or an SSH/askpass/proxy command.
        "GIT_REPLACE_REF_BASE",
        "GIT_SHALLOW_FILE",
        "GIT_CEILING_DIRECTORIES",
        "GIT_SSH_COMMAND",
        "GIT_SSH",
        "GIT_ASKPASS",
        "GIT_PROXY_COMMAND",
        "GIT_EXEC_PATH",
        "JUST_ALLOW_MISSING",
        "JUST_NO_DEPS",
        "JUST_WORKING_DIRECTORY",
        "JUST_DOTENV_COMMAND",
        "CDPATH",
        "IFS",
        "SHELLOPTS",
        "BASHOPTS",
        "POSIXLY_CORRECT",
        "SHELL",
    ] {
        cmd.env_remove(var);
    }
    // ISOLATE global/system config: REMOVING the override variables would
    // restore git's normal lookup of the caller's `$HOME/.gitconfig` /
    // `$XDG_CONFIG_HOME/git/config`, which could carry a hostile alias
    // (`[alias] ci-verdict = !true`) that a plan's last `git` command runs.
    // Pointing them at /dev/null (an empty trusted file) makes the plan's git
    // see only the repo's OWN local config.
    cmd.env("GIT_CONFIG_GLOBAL", "/dev/null");
    cmd.env("GIT_CONFIG_SYSTEM", "/dev/null");
    sanitize_loader_env(cmd);
    // `std::env::vars()` PANICS on a non-UTF-8 key/value; enumerate with
    // `vars_os` and match the ASCII `BASH_FUNC_*%%` names byte-preservingly
    // (a non-UTF-8 name whose ASCII `BASH_FUNC_`/`%%` edges still match is
    // removed — bash's raw env parser accepts such byte-named functions).
    for (k, _) in std::env::vars_os() {
        let name = k.to_string_lossy();
        if name.starts_with("BASH_FUNC_") && name.ends_with("%%") {
            cmd.env_remove(&k);
        }
    }
}

/// Resolve the `just` runner to a TRUSTED ABSOLUTE path (F-027) so a caller
/// cannot prepend a `just` shim to `PATH` and fake a green Check. Searches the
/// system install locations, then the operator's own `~/.cargo/bin` /
/// `~/.local/bin` (the common cargo install site the fallback needs). `HOME`
/// is a single-user TRUSTED-OPERATOR residual: like the recipe-shell `PATH`,
/// the operator controls their own environment, and in this single-user local
/// tool there is no separate-principal boundary to cross. An install not found
/// is a spawn failure (honest), never a caller-resolved binary.
fn resolve_just() -> Result<std::path::PathBuf, String> {
    // Phase 1 — system install locations (this probe reads no caller env). If
    // no runnable `just` is found here, fall through to the operator's own
    // `~/.cargo/bin` / `~/.local/bin` below. An install found nowhere is a
    // spawn failure (honest), never a caller-resolved `PATH` binary.
    for d in ["/usr/bin", "/usr/local/bin", "/opt/homebrew/bin"] {
        let p = std::path::PathBuf::from(d).join("just");
        if p.is_file()
            && std::process::Command::new(&p)
                .arg("--version")
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
        {
            return Ok(p);
        }
    }
    // Trust the operator's own install locations (`~/.cargo/bin`,
    // `~/.local/bin`) — single-user local tool, so the operator is the trusted
    // principal; `~/.cargo/bin` is the common Homebrew/cargo install site that
    // the `just check` fallback must find (system-only would break it). Never a
    // bare `just` on `PATH` (which a caller could shim).
    let home = std::env::var_os("HOME")
        .ok_or_else(|| "no trusted `just` found (HOME not set)".to_string())?;
    // A RELATIVE `HOME` resolves against whatever the child's cwd is (the
    // disposable worktree), so the checked file and the executed file could
    // differ (a PR could ship its own `.cargo/bin/just`). `HOME` must be
    // absolute.
    let home = std::path::Path::new(&home);
    if !home.is_absolute() {
        return Err("no trusted `just` found (relative HOME)".to_string());
    }
    for sub in [".cargo/bin", ".local/bin"] {
        let p = home.join(sub).join("just");
        if p.is_file()
            && std::process::Command::new(&p)
                .arg("--version")
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
        {
            return Ok(p);
        }
    }
    Err("no trusted `just` found in system or operator install locations".to_string())
}

/// Run the repo CI plan inside the temp worktree (the PR's own commits).
///
/// F-013 first materializes the exact immutable blob bytes into the worktree,
/// so a smudge/attribute filter cannot replace the plan. The plan's
/// stdout/stderr are redirected to the null device (never buffered in memory),
/// so a high-volume plan cannot grow git-forge's heap. The wait is bounded
/// (`timeout`); on expiry (F-015) the WHOLE process group is killed and reaped
/// (not just the direct child), so a background descendant whose leader exits
/// cannot survive the deadline. Returns the exit status, whether the deadline
/// was hit (F-002/F-015), and a `detail` reason for non-green runs that are
/// neither a plain non-zero exit nor a timeout.
///
/// KNOWN LIMITATION: a descendant that creates its own new session/process
/// group (e.g. a plan that `setsid`s a long-lived job) leaves the original
/// group, so the `pgid_alive` probe sees an empty group and the run is treated
/// as completed/detached — the escaped descendant is not killed by the
/// deadline and survives the worktree cleanup. Process-group signalling is
/// containment-only-within-same-group; full containment of a daemonizing plan
/// requires OS-level isolation (container/chroot), which is out of scope for
/// this single-user local tool.
///
/// Related residual: if the group-KILL itself fails (e.g. EPERM from a
/// process-quota restriction on the direct `kill(2)` syscall), a same-group
/// descendant is not terminated. The run is always recorded FAILED (never
/// green) in that
/// case — the leader-exit path only returns success when the group is PROVEN
/// empty — but the descendant may survive. This is fail-closed, not a false
/// green; fully bounding it needs the same OS-level containment.
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
            detail: None,
        };
    }
    let (prog, jf_arg): (std::path::PathBuf, Option<std::path::PathBuf>) = match plan {
        CiPlan::CiSh => (std::path::PathBuf::from("/bin/bash"), None),
        CiPlan::JustCheck(name) => {
            let jf = worktree.join(name);
            // F-027: resolve `just` to a TRUSTED ABSOLUTE path so a caller
            // cannot prepend a `just` shim to `PATH` and fake a green Check.
            let just = match resolve_just() {
                Ok(j) => j,
                Err(_) => {
                    // `just` is not at a trusted system location: an honest spawn
                    // failure (recorded as a failed Check), never a
                    // caller-resolved binary.
                    return CiRun {
                        status: None,
                        timed_out: false,
                        detail: None,
                    };
                }
            };
            (just, Some(jf))
        }
    };
    let mut cmd = std::process::Command::new(&prog);
    // F-027: Just's default recipe shell is `sh` resolved through PATH; a PATH
    // shim `sh` could exit 0 for a failing recipe. Pin the recipe shell to the
    // trusted `/bin/sh` so the plan's own content decides. The program and
    // justfile paths are passed as native `PathBuf` (NOT `to_string_lossy`), so
    // a non-UTF-8 HOME/install path never resolves to a different bytes' worth
    // of executable (decision record: non-UTF-8 -> honest failed Check).
    match &jf_arg {
        Some(jf) => {
            cmd.arg("--justfile")
                .arg(jf)
                .arg("--shell")
                .arg("/bin/sh")
                .arg("check");
        }
        None => {
            cmd.arg(".forge/ci.sh");
        }
    }
    cmd.current_dir(worktree)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        // F-027: keep `JUST_DRY_RUN` forced off so a caller cannot skip a
        // failing recipe; the rest of the caller's env is tightened below.
        .env("JUST_DRY_RUN", "false")
        .process_group(0); // the child leads its own process group (pgid = pid)
                           // F-027: drop the control-env vectors a caller could use to redirect or
                           // fake the plan — Git location overrides, Bash exported functions, and the
                           // shell control vars — so the plan necessarily runs against the snapshot.
    tighten_ci_env(&mut cmd);
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(_) => {
            return CiRun {
                status: None,
                timed_out: false,
                detail: None,
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
                match pgid_alive(pgid) {
                    Ok(true) => {
                        // Descendants are alive: kill the group and FAIL the
                        // run (a survivor that cannot be proven dead must not
                        // be recorded green).
                        let _ = signal_pg(pgid, libc::SIGKILL);
                        let _ = child.wait();
                        return CiRun {
                            status: None,
                            timed_out: false,
                            detail: Some("left a background process running"),
                        };
                    }
                    Ok(false) => {
                        return CiRun {
                            status: st.code(),
                            timed_out: false,
                            detail: None,
                        };
                    }
                    Err(_) => {
                        // Cannot prove the group is empty — a descendant may
                        // still be alive, so the run is NOT completed. This is
                        // NOT a deadline timeout: the leader exited on its own,
                        // but group liveness could not be proven (an errno
                        // other than ESRCH), so the deadline label does not
                        // apply. Fail containment: signal the group then
                        // bounded-reap so a descendant is not left running while
                        // the worktree is removed.
                        let _ = signal_pg(pgid, libc::SIGKILL);
                        let _ = reap_with_grace(&mut child);
                        return CiRun {
                            status: None,
                            timed_out: false,
                            detail: Some("could not prove the process group is empty"),
                        };
                    }
                }
            }
            Ok(None) => {}
            Err(_) => {
                // A transient wait failure (e.g. ECHILD/race) does not mean the
                // group is empty — the plan may still be running. Fail CLOSED:
                // signal the whole group then bounded-reap, so a surviving
                // descendant is not left running while the worktree is removed.
                let _ = signal_pg(pgid, libc::SIGKILL);
                let _ = reap_with_grace(&mut child);
                return CiRun {
                    status: None,
                    timed_out: false,
                    detail: Some("could not prove the process group is empty"),
                };
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    // F-015/F-024: kill+reap the COMPLETE process group via a direct `kill(2)`
    // syscall so a background descendant cannot survive the bounded deadline.
    // The kill RESULT is checked: on failure (e.g. EPERM under a restrictive
    // process quota) the child may still be running, so the reap is bounded —
    // an unbounded `child.wait()` on a live child would hang `ci run` and
    // leave the run unrecorded.
    // The kill RESULT is not a signal of completion either way: even a
    // SUCCESSFUL signal does not prove prompt termination (a child in an
    // uninterruptible kernel wait can remain unreaped), and a FAILED signal
    // (e.g. the user's process quota is exhausted) leaves the child running —
    // so the reap is bounded in both cases (never hang without a Check).
    let _ = signal_pg(pgid, libc::SIGKILL);
    let status = reap_with_grace(&mut child);
    CiRun {
        status,
        timed_out: true,
        detail: None,
    }
}

/// Bound the reap of a child whose group kill failed: poll `try_wait` for a
/// short grace window (so a still-running child cannot hang the run), then
/// return the reaped status (or `None` if it did not exit in time).
fn reap_with_grace(child: &mut std::process::Child) -> Option<i32> {
    for _ in 0..25 {
        match child.try_wait() {
            Ok(Some(st)) => return st.code(),
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(_) => return None,
        }
    }
    // The child is still alive at the end of the grace window: SIGKILL it so it
    // is not leaked, then BOUND the reap — signal acceptance does not prove
    // prompt termination (a process in an uninterruptible kernel wait can
    // remain unreaped), so an unbounded `child.wait()` could still hang.
    let _ = child.kill();
    for _ in 0..25 {
        match child.try_wait() {
            Ok(Some(st)) => return st.code(),
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(_) => return None,
        }
    }
    None
}

#[cfg(test)]
mod tests;
