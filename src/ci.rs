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
    if plan.label == "just check" {
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
/// `sig` via a TRUSTED absolute `/bin/kill` (F-024): a PATH shim must not be
/// able to no-op termination, and `--` is passed so negative-pgid parsing is
/// portable. Returns an error carrying stderr on failure.
fn signal_pg(pgid: i32, sig: &str) -> Result<(), String> {
    let mut cmd = std::process::Command::new("/bin/kill");
    cmd.arg("-s")
        .arg(sig)
        .arg("--")
        .arg(format!("-{pgid}"))
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped());
    tighten_kill_env(&mut cmd);
    let out = cmd.output();
    match out {
        Ok(o) if o.status.success() => Ok(()),
        Ok(o) => Err(String::from_utf8_lossy(&o.stderr).trim().to_string()),
        Err(e) => Err(format!("failed to spawn /bin/kill: {e}")),
    }
}

/// True if any process in the group `pgid` is still alive (kill(-pgid, 0)
/// probe via trusted `/bin/kill`), OR an error if the group's liveness CANNOT
/// be proven (a spawn failure, or an unexpected exit code). Used by F-015 to
/// prove the process group is empty before a leader-exit is treated as a
/// completed run: an unprovable probe must FAIL the run, never assume the
/// group is empty (a descendant could still be alive).
fn pgid_alive(pgid: i32) -> Result<bool, String> {
    let mut cmd = std::process::Command::new("/bin/kill");
    cmd.arg("-0")
        .arg("--")
        .arg(format!("-{pgid}"))
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped());
    tighten_kill_env(&mut cmd);
    let out = cmd.output();
    match out {
        Ok(o) if o.status.success() => Ok(true),
        // `kill(2)` returns ESRCH for an empty group and EPERM for an EXISTING
        // but unsignalable group — both surface as a non-zero exit, so read the
        // diagnostic to distinguish them: only ESRCH ("No such process") proves
        // the group is empty; EPERM/anything else is "cannot prove empty" and
        // fails closed.
        Ok(o) => {
            let err = String::from_utf8_lossy(&o.stderr).to_string();
            if err.contains("No such process") || err.contains("no such process") {
                Ok(false)
            } else {
                Err(format!(
                    "kill -0 -{pgid} failed (cannot prove the group is empty): {err}"
                ))
            }
        }
        Err(e) => Err(format!("failed to spawn /bin/kill: {e}")),
    }
}

/// F-027: remove the caller-env vectors that could redirect or fake the
/// snapshot plan in a subprocess. `BASH_ENV`/`ENV` and the Bash exported
/// functions (`BASH_FUNC_*%%`) can inject statements that exit before the
/// plan's failing line; the Git location overrides (`GIT_DIR`,
/// `GIT_WORK_TREE`, `GIT_INDEX_FILE`, `GIT_OBJECT_DIRECTORY`,
/// `GIT_ALTERNATE_OBJECT_DIRECTORIES` and the Git config knobs) redirect a
/// plan `git` to a different repository/worktree/index; `CDPATH`/`IFS`/`SHELL`
/// alter path/word splitting. The plan interpreter is invoked by absolute path
/// (`/bin/bash` for `.forge/ci.sh`, a trusted absolute path for `just`) so a
/// `PATH` shim cannot replace it.
/// Sanitize a `/bin/kill` Command (F-027): a caller loader constructor
/// (`LD_PRELOAD`/`LD_TRACE_LOADED_OBJECTS`, …) could otherwise make `kill` exit
/// 0 without signalling, so the deadline path hangs or the leader-exit branch
/// leaks a descendant. Also forces `LC_ALL=C` so the `kill -0` ESRCH diagnostic
/// is the stable English "No such process" (locale-independent).
fn tighten_kill_env(cmd: &mut std::process::Command) {
    sanitize_loader_env(cmd);
    cmd.env("LC_ALL", "C");
}

/// Remove the dynamic-loader injection variables (`LD_*`/`DYLD_*`) from a
/// Command: a caller-supplied constructor can otherwise run before the trusted
/// program and exit 0 or alter its behavior. Shared by the plan interpreter,
/// `/bin/kill`, and the CI lifecycle `git` (F-027).
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
    // ONLY system install locations: a caller cannot influence the result
    // through `PATH`, `HOME`, or any other environment value. An install not
    // at one of these is a spawn failure (honest), never a caller-resolved
    // binary.
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
/// cannot survive the deadline. Returns the exit status and whether the
/// deadline was hit (F-002/F-015).
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
/// Related residual: if the group-KILL itself fails (e.g. under process-quota
/// exhaustion the trusted `/bin/kill` cannot spawn), a same-group descendant
/// is not terminated. The run is always recorded FAILED (never green) in that
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
        };
    }
    let (prog, jf_arg): (std::path::PathBuf, Option<std::path::PathBuf>) = match plan.label {
        ".forge/ci.sh" => (std::path::PathBuf::from("/bin/bash"), None),
        "just check" => {
            let jf = plan
                .justfile
                .as_ref()
                .map(|p| worktree.join(p))
                .unwrap_or_else(|| worktree.join("justfile"));
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
                    };
                }
            };
            (just, Some(jf))
        }
        _ => (std::path::PathBuf::from("/bin/bash"), None),
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
                        let _ = signal_pg(pgid, "KILL");
                        let _ = child.wait();
                        return CiRun {
                            status: None,
                            timed_out: true,
                        };
                    }
                    Ok(false) => {
                        return CiRun {
                            status: st.code(),
                            timed_out: false,
                        };
                    }
                    Err(_) => {
                        // Cannot prove the group is empty — a descendant may
                        // still be alive, so the run is NOT completed. This is
                        // NOT a deadline timeout: the leader exited on its own,
                        // but group liveness could not be proven (a probe or
                        // spawn failure), so the deadline-integrity label does
                        // not apply. Fail containment: signal the group then
                        // bounded-reap so a descendant is not left running while
                        // the worktree is removed.
                        let _ = signal_pg(pgid, "KILL");
                        let _ = reap_with_grace(&mut child);
                        return CiRun {
                            status: None,
                            timed_out: false,
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
                let _ = signal_pg(pgid, "KILL");
                let _ = reap_with_grace(&mut child);
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
    // survive the bounded deadline. The kill RESULT is checked: on failure
    // (e.g. the user's process quota is exhausted) the child may still be
    // running, so the reap is bounded — an unbounded `child.wait()` on a live
    // child would hang `ci run` and leave the run unrecorded.
    // The kill RESULT is not a signal of completion either way: even a
    // SUCCESSFUL signal does not prove prompt termination (a child in an
    // uninterruptible kernel wait can remain unreaped), and a FAILED signal
    // (e.g. the user's process quota is exhausted) leaves the child running —
    // so the reap is bounded in both cases (never hang without a Check).
    let _ = signal_pg(pgid, "KILL");
    let status = reap_with_grace(&mut child);
    CiRun {
        status,
        timed_out: true,
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
