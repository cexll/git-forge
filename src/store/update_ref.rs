//! The one `git update-ref --stdin` runner. Every multi-ref CAS goes through
//! it as a start/prepare/commit transaction, driven step by step so that a
//! check can run between `prepare` and `commit` while git holds every lock.
//!
//! Every exit path finishes the child the same way ([`UpdateRef`]'s `Drop`):
//! stdin is closed, which makes git abort an open or prepared transaction
//! and remove its `.lock` files, and the child is reaped. stderr is drained
//! by a thread from spawn onward, so output from git or a
//! `reference-transaction` hook can never fill the pipe and stall git
//! before it answers `prepare: ok`.
use super::*;
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, ChildStdout, Stdio};
use std::thread::JoinHandle;

/// A running `git update-ref --stdin`.
struct UpdateRef {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    stderr: Option<JoinHandle<Vec<u8>>>,
}

impl UpdateRef {
    fn spawn(git_dir: &Path) -> Result<Self, StoreError> {
        let mut child = Command::new("git")
            .arg("--git-dir")
            .arg(git_dir)
            .arg("update-ref")
            .arg("--stdin")
            .env("LC_ALL", "C")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| StoreError::Command(e.to_string()))?;
        let mut stderr = child.stderr.take().expect("piped stderr");
        let stderr = std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = stderr.read_to_end(&mut buf);
            buf
        });
        Ok(Self {
            stdin: child.stdin.take(),
            stdout: BufReader::new(child.stdout.take().expect("piped stdout")),
            stderr: Some(stderr),
            child,
        })
    }

    /// Send `text`; `false` when git no longer reads (it exited).
    fn send(&mut self, text: &str) -> bool {
        self.stdin.as_mut().is_some_and(|stdin| {
            stdin
                .write_all(text.as_bytes())
                .and_then(|()| stdin.flush())
                .is_ok()
        })
    }

    /// Wait for `prepare: ok`. Only `start: ok` may precede it; any other
    /// line, end of output, or a read error means not prepared.
    fn prepared(&mut self) -> bool {
        let mut reply = String::new();
        loop {
            reply.clear();
            match self.stdout.read_line(&mut reply) {
                Ok(0) | Err(_) => return false,
                Ok(_) if reply == "start: ok\n" => {}
                Ok(_) => return reply == "prepare: ok\n",
            }
        }
    }

    /// Close stdin (git aborts anything not committed), drain stdout, reap
    /// the child, and collect stderr.
    fn finish(&mut self) -> (Option<std::process::ExitStatus>, String) {
        drop(self.stdin.take());
        let _ = std::io::copy(&mut self.stdout, &mut std::io::sink());
        let status = self.child.wait().ok();
        let stderr = self
            .stderr
            .take()
            .and_then(|reader| reader.join().ok())
            .unwrap_or_default();
        (status, String::from_utf8_lossy(&stderr).trim().to_string())
    }
}

impl Drop for UpdateRef {
    /// An early return or a panic still aborts and reaps.
    fn drop(&mut self) {
        if self.stderr.is_some() {
            self.finish();
        }
    }
}

impl EventStore {
    /// Run `git update-ref --stdin` with the given transaction lines.
    /// Returns Ok(()) when every command applied atomically, Err otherwise
    /// (stderr included in the error). Never swallows failures.
    pub(crate) fn run_update_ref_stdin(&self, lines: &[String]) -> Result<(), StoreError> {
        self.run_update_ref_transaction(lines, &[])
    }

    /// [`Self::run_update_ref_stdin`] that also requires each ref in
    /// `absent` to still not exist once `prepare` holds every lock. An
    /// expected-absent `option no-deref` update accepts a dangling symbolic
    /// ref and replaces it, so the check runs between `prepare` and
    /// `commit`: the lock on each ref is held there, and any other git writer
    /// needs that same lock. A present ref aborts the whole transaction.
    ///
    /// Exit paths: spawn failure (nothing started); a write error on stdin,
    /// end of output, a read error, or an unexpected reply before
    /// `prepare: ok` (not prepared: `abort`, finish, git's stderr as the
    /// error); a present `absent` ref after prepare (`abort`, finish, a
    /// named error); `commit` (finish, exit status decides). A panic or an
    /// early return finishes the child in `Drop`.
    pub(crate) fn run_update_ref_transaction(
        &self,
        lines: &[String],
        absent: &[String],
    ) -> Result<(), StoreError> {
        let mut git = UpdateRef::spawn(&self.git_dir)?;
        let mut request = String::from("start\n");
        for line in lines {
            request.push_str(line);
            request.push('\n');
        }
        request.push_str("prepare\n");
        let prepared = git.send(&request) && git.prepared();
        let present = absent
            .iter()
            .find(|name| prepared && self.ref_present(name));
        let committing = prepared && present.is_none();
        git.send(if committing { "commit\n" } else { "abort\n" });
        let (status, stderr) = git.finish();
        if let Some(name) = present {
            return Err(StoreError::Command(format!(
                "{name} exists at commit time; transaction aborted"
            )));
        }
        match status {
            Some(status) if committing && status.success() => Ok(()),
            _ if stderr.is_empty() => Err(StoreError::Command(
                "git update-ref --stdin did not commit".into(),
            )),
            _ => Err(StoreError::Command(stderr)),
        }
    }

    /// Whether `name` exists in any form: direct, symbolic (even dangling),
    /// or an unreadable ref file.
    fn ref_present(&self, name: &str) -> bool {
        !matches!(self.repo.find_reference(name), Err(e) if e.code() == git2::ErrorCode::NotFound)
    }
}
