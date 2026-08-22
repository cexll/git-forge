//! Store layer (t1a): git-ref event store on top of the pure core.
//!
//! Stores events as git commits under `refs/forge/*`, allocates sequential ids
//! via a versioned counter commit chain with CAS, and reads entity chains.
//! Git objects (commits/blobs/trees) are written via libgit2 (`git2`); ref
//! updates — where the wire contract demands `update-ref <ref> <new> <old>`
//! CAS — go through the `git update-ref --stdin` binary, the only primitive
//! that atomically and threshold-checked moves refs.
//!
//! Wire contract: `docs/architecture/git-forge.md` (§ Ref Layout, § Event
//! Commit Layout, § Deterministic Single-Chain CAS Append, § Lazy
//! initialization / Allocation is atomic).

use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

use git2::{Commit, Error as GitError, Oid, Repository, Signature, TreeBuilder, TreeEntry};

use crate::event::{Event, EventKind, JsonValue};

/// The single mutable counter ref holding the next sequential id.
pub const COUNTER_REF: &str = "refs/forge/meta/counter";

/// Genesis commit message for empty entity-chain roots (no event payload).
const GENESIS_MSG: &str = "forge:genesis";

/// Number of bounded CAS append retries (wire contract default).
const MAX_APPEND_RETRIES: u32 = 3;

/// Number of bounded allocation retries on counter collision.
const MAX_ALLOC_RETRIES: u32 = 3;

/// All-zero OID used by `git update-ref` to mean "ref must not exist".
const ZERO_OID: &str = "0000000000000000000000000000000000000000";

/// Entity ref path helpers (wire contract § Ref Layout).
pub fn issue_ref(n: u64) -> String {
    format!("refs/forge/issues/{n}")
}
pub fn pr_head_ref(n: u64) -> String {
    format!("refs/forge/prs/{n}/head")
}
pub fn pr_meta_ref(n: u64) -> String {
    format!("refs/forge/prs/{n}/meta")
}
pub fn pr_source_ref(n: u64) -> String {
    format!("refs/forge/prs/{n}/source")
}
pub fn pr_base_ref(n: u64) -> String {
    format!("refs/forge/prs/{n}/base")
}
pub fn pr_result_ref(n: u64) -> String {
    format!("refs/forge/prs/{n}/result")
}

/// Store errors. `CasConflict`/`Exhausted` are the retry-signalling failures;
/// `RefExists` guards entity creation collisions.
#[derive(Debug)]
pub enum StoreError {
    Git(GitError),
    CasConflict,
    Exhausted,
    MissingEvent,
    MissingRef,
    RefExists(String),
    Command(String),
    InvalidState(String),
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoreError::Git(e) => write!(f, "git error: {e}"),
            StoreError::CasConflict => write!(f, "ref CAS conflict — ref moved, retry"),
            StoreError::Exhausted => write!(f, "bounded CAS retries exhausted; refs unchanged"),
            StoreError::MissingEvent => write!(f, "entity chain has no event.json payload"),
            StoreError::MissingRef => write!(f, "required ref does not exist"),
            StoreError::RefExists(r) => write!(f, "entity ref already exists: {r}"),
            StoreError::Command(s) => write!(f, "git command failed: {s}"),
            StoreError::InvalidState(s) => write!(f, "invalid store state: {s}"),
        }
    }
}

impl std::error::Error for StoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            StoreError::Git(e) => Some(e),
            _ => None,
        }
    }
}

impl From<GitError> for StoreError {
    fn from(e: GitError) -> Self {
        StoreError::Git(e)
    }
}

/// Git-ref event store — read-only surface. All forge writes require binding
/// an explicit committer identity via [`EventStore::bind_signature`], which
/// yields a [`BoundEventStore`]; unbound stores have no signature, so a
/// direct caller cannot accidentally write forge commits with a fallback
/// identity (VAL-115 STAGE A: libgit2 config reads after open can SIGSEGV).
pub struct EventStore {
    repo: Repository,
    git_dir: std::path::PathBuf,
}

/// A store with an explicitly bound committer identity — the ONLY route to
/// forge write methods. Created from [`EventStore::bind_signature`]; reads
/// nothing from libgit2 config at write time (the caller resolves identity
/// via the git binary), so a config corrupted after `Repository::open` cannot
/// page-fault this path.
pub struct BoundEventStore {
    /// The underlying read-capable store.
    inner: EventStore,
    /// Explicit committer identity for every forge commit written through this
    /// bound store. Never read via `repo.signature()`.
    signature: Signature<'static>,
}

impl EventStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        let repo = Repository::open(path.as_ref())?;
        Self::from_repo(repo)
    }

    pub fn init(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        let repo = Repository::init(path.as_ref())?;
        Self::from_repo(repo)
    }

    fn from_repo(repo: Repository) -> Result<Self, StoreError> {
        // Resolve the actual git dir so `git --git-dir` runs against the right
        // repository (handles linked worktrees / .git gitfiles).
        let path_str = repo
            .path()
            .to_str()
            .ok_or_else(|| StoreError::InvalidState("non-UTF8 .git path".into()))?
            .to_string();
        Ok(EventStore {
            repo,
            git_dir: std::path::PathBuf::from(path_str),
        })
    }

    /// Bind an explicitly resolved committer identity to this store, yielding
    /// a write-capable [`BoundEventStore`]. The ONLY route to forge writes —
    /// `open`/`init` are read-only-capable and carry no signature, so a
    /// mutating call must choose an identity deliberately.
    pub fn bind_signature(self, sig: Signature<'static>) -> BoundEventStore {
        BoundEventStore {
            inner: self,
            signature: sig,
        }
    }

    /// Run `git update-ref --stdin` with the given transaction lines.
    /// Returns Ok(()) when every command applied atomically, Err otherwise
    /// (stderr included in the error). Never swallows failures.
    fn run_update_ref_stdin(&self, lines: &[String]) -> Result<(), StoreError> {
        use std::io::Write;
        let mut child = Command::new("git")
            .arg("--git-dir")
            .arg(&self.git_dir)
            .arg("update-ref")
            .arg("--stdin")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| StoreError::Command(e.to_string()))?;
        let mut stdin = child.stdin.take().unwrap();
        let mut buf = String::from("start\n");
        for l in lines {
            buf.push_str(l);
            buf.push('\n');
        }
        buf.push_str("prepare\ncommit\n");
        let _ = stdin.write_all(buf.as_bytes());
        drop(stdin);
        let out = child
            .wait_with_output()
            .map_err(|e| StoreError::Command(e.to_string()))?;
        if out.status.success() {
            Ok(())
        } else {
            let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
            Err(StoreError::Command(stderr))
        }
    }

    /// CAS one ref: move `entity_ref` from `expected` to `new_oid` atomically.
    /// - Ok(true): applied.
    /// - Ok(false): CAS conflict — the ref moved away from `expected` (or
    ///   appeared when `expected` was None). The caller retries with a fresh
    ///   read.
    /// - Err: a real git failure (the ref still holds `expected`).
    fn cas_update_ref(
        &self,
        entity_ref: &str,
        expected: Option<Oid>,
        new_oid: Oid,
    ) -> Result<bool, StoreError> {
        let old = expected
            .map(|o| o.to_string())
            .unwrap_or_else(|| ZERO_OID.to_string());
        let line = format!("update {entity_ref} {new_oid} {old}");
        match self.run_update_ref_stdin(&[line]) {
            Ok(()) => Ok(true),
            Err(e) => {
                // Classify: CAS conflict when the ref no longer holds the
                // expected old value; otherwise a genuine git failure.
                let cur = self.current_tip(entity_ref)?;
                if cur != expected {
                    Ok(false)
                } else {
                    Err(e)
                }
            }
        }
    }

    fn current_tip(&self, entity_ref: &str) -> Result<Option<Oid>, StoreError> {
        match self.repo.find_reference(entity_ref) {
            Ok(r) => Ok(r.target()),
            Err(e) if e.code() == git2::ErrorCode::NotFound => Ok(None),
            Err(e) => Err(StoreError::Git(e)),
        }
    }

    /// Read a single entity event chain oldest→tip. Skips commits whose tree
    /// has no `.forge/event.json` (genesis roots and L2 merge nodes).
    pub fn read_chain(&self, entity_ref: &str) -> Result<Vec<Event>, StoreError> {
        let mut out: Vec<Event> = Vec::new();
        let Some(tip) = self.current_tip(entity_ref)? else {
            return Ok(out);
        };
        let mut oid = tip;
        loop {
            let commit = self.repo.find_commit(oid)?;
            if let Some(event) = self.read_event_blob(&commit)? {
                out.push(event);
            }
            match commit.parent_ids().next() {
                Some(p) => oid = p,
                None => break,
            }
        }
        out.reverse();
        Ok(out)
    }

    fn read_event_blob(&self, commit: &Commit) -> Result<Option<Event>, StoreError> {
        let tree = self.repo.find_tree(commit.tree_id())?;
        match tree.get_path(std::path::Path::new(".forge/event.json")) {
            Ok(entry) => {
                let obj = entry.to_object(&self.repo)?;
                match obj.as_blob() {
                    Some(blob) => {
                        let content = std::str::from_utf8(blob.content()).map_err(|_| {
                            StoreError::InvalidState("event.json not valid UTF-8".into())
                        })?;
                        Event::from_json(content)
                            .map(Some)
                            .ok_or(StoreError::MissingEvent)
                    }
                    None => Err(StoreError::InvalidState(
                        ".forge/event.json is not a blob".into(),
                    )),
                }
            }
            Err(e) if e.code() == git2::ErrorCode::NotFound => Ok(None),
            Err(e) => Err(StoreError::Git(e)),
        }
    }

    fn read_counter_next(&self, counter_tip: Oid) -> Result<u64, StoreError> {
        let commit = self.repo.find_commit(counter_tip)?;
        let tree = self.repo.find_tree(commit.tree_id())?;
        let entry = tree
            .get_path(std::path::Path::new(".forge/counter.json"))
            .map_err(|_| StoreError::InvalidState("counter tree missing counter.json".into()))?;
        self.counter_next_from_entry(&entry)
    }

    /// The single public counter-read entry: read `next` from the counter
    /// commit chain tip. An absent counter (fresh repo) reads as 1, so callers
    /// can bound entity scans without distinguishing "no counter" from an
    /// empty one. The whole chain walk reuses the private read internals.
    pub fn counter_next(&self) -> Result<u64, StoreError> {
        match self.current_tip(COUNTER_REF)? {
            Some(tip) => self.read_counter_next(tip),
            None => Ok(1),
        }
    }

    fn counter_next_from_entry(&self, entry: &TreeEntry<'_>) -> Result<u64, StoreError> {
        let obj = entry.to_object(&self.repo)?;
        let blob = obj
            .as_blob()
            .ok_or_else(|| StoreError::InvalidState("counter.json is not a blob".into()))?;
        let content = std::str::from_utf8(blob.content())
            .map_err(|_| StoreError::InvalidState("counter.json not valid UTF-8".into()))?;
        parse_counter_next(content)
    }

    /// Low-level accessor for tests and the CLI layer.
    pub fn repo(&self) -> &Repository {
        &self.repo
    }
}

impl BoundEventStore {
    /// The underlying read-capable store — for ref/existence/read validation
    /// during a mutation (merge gates, existence checks). Write methods stay
    /// on `BoundEventStore`; this is delegation, not a second write surface.
    pub fn store(&self) -> &EventStore {
        &self.inner
    }

    /// The underlying repository (delegation for CLI ref/gate validation).
    pub fn repo(&self) -> &Repository {
        &self.inner.repo
    }

    /// Read a single entity event chain (delegation to the read surface).
    pub fn read_chain(&self, entity_ref: &str) -> Result<Vec<Event>, StoreError> {
        self.inner.read_chain(entity_ref)
    }

    /// Read the counter next value (delegation to the read surface).
    pub fn counter_next(&self) -> Result<u64, StoreError> {
        self.inner.counter_next()
    }

    /// Write a commit whose tree holds one blob at `.forge/<leaf>`; returns its
    /// OID without touching any ref (CAS is done by the caller).
    ///
    /// git2 `TreeBuilder::insert` takes a single path component, so the `.forge`
    /// subtree is built first and inserted into the root tree as
    /// `FileMode::Tree` (wire contract § Event Commit Layout).
    fn write_forge_commit(
        &self,
        leaf: &str,
        content: &[u8],
        parents: &[Oid],
        message: &str,
    ) -> Result<Oid, StoreError> {
        let blob = self.inner.repo.blob(content)?;
        let mut forge_tb: TreeBuilder = self.inner.repo.treebuilder(None)?;
        // git2 0.20 TreeBuilder::insert takes i32; 0o100644 = regular blob.
        forge_tb.insert(leaf, blob, 0o100644)?;
        let forge_tree = self.inner.repo.find_tree(forge_tb.write()?)?;
        let mut root_tb: TreeBuilder = self.inner.repo.treebuilder(None)?;
        // 0o040000 = git tree entry.
        root_tb.insert(".forge", forge_tree.id(), 0o040000)?;
        let tree = self.inner.repo.find_tree(root_tb.write()?)?;
        let sig = &self.signature;
        let parent_commits: Vec<Commit> = parents
            .iter()
            .map(|oid| self.inner.repo.find_commit(*oid))
            .collect::<Result<_, _>>()?;
        let parent_refs: Vec<&Commit> = parent_commits.iter().collect();
        let oid = self
            .inner
            .repo
            .commit(None, sig, sig, message, &tree, &parent_refs)?;
        Ok(oid)
    }

    fn write_event_commit(
        &self,
        event: &Event,
        parents: &[Oid],
        message: &str,
    ) -> Result<Oid, StoreError> {
        self.write_forge_commit("event.json", event.to_json().as_bytes(), parents, message)
    }

    fn write_counter_commit(&self, next: u64, parent: Option<Oid>) -> Result<Oid, StoreError> {
        let value = format!("{{\"v\":1,\"next\":{next}}}");
        let parents: Vec<Oid> = parent.into_iter().collect();
        self.write_forge_commit("counter.json", value.as_bytes(), &parents, "forge:counter")
    }

    /// Genesis root for a fresh entity chain (empty tree).
    fn genesis_oid(&self) -> Result<Oid, StoreError> {
        let tree_oid = self.inner.repo.treebuilder(None)?.write()?;
        let tree = self.inner.repo.find_tree(tree_oid)?;
        let sig = &self.signature;
        let oid = self
            .inner
            .repo
            .commit(None, sig, sig, GENESIS_MSG, &tree, &[])?;
        Ok(oid)
    }

    /// Append an event to `entity_ref` with a true CAS. The event commit's sole
    /// parent is the observed tip; the ref moves only if it still points at
    /// that tip (`git update-ref <ref> <new> <old>`). On CAS conflict, retry
    /// with the NEW tip as sole parent while retaining the same UUID, bounded
    /// by `MAX_APPEND_RETRIES`. Returns the new tip OID.
    pub fn append_event(&self, entity_ref: &str, event: &Event) -> Result<Oid, StoreError> {
        let mut tip = self.inner.current_tip(entity_ref)?;
        for _ in 0..MAX_APPEND_RETRIES {
            let message = format!("forge:{}:{}", event.kind.as_str(), event.entity_id);
            let parents: Vec<Oid> = tip.into_iter().collect();
            let new_oid = self.write_event_commit(event, &parents, &message)?;
            match self.inner.cas_update_ref(entity_ref, tip, new_oid)? {
                true => return Ok(new_oid),
                false => {
                    // CAS conflict: another writer won. Re-read the new tip and
                    // retry with the SAME event (retained UUID), re-parenting.
                    tip = self.inner.current_tip(entity_ref)?;
                }
            }
        }
        Err(StoreError::Exhausted)
    }

    /// Allocate the next sequential id via a single atomic ref transaction,
    /// retrying on counter collision with a fresh read (bounded).
    ///
    /// Fresh repo (absent counter): create counter commit `{v:1,next:2}` AND
    /// genesis for `refs/forge/issues/1` in one transaction, return 1.
    /// Existing counter: read `next`, write `{v:1,next:next+1}`, CAS the counter
    /// (expected old tip) and create the new entity genesis in one transaction.
    ///
    /// Classification on a failed batch, by re-reading the COUNTER first:
    ///   - counter moved away from our expectation → a concurrent allocator won
    ///     this id → RETRY with a fresh read (distinct sequential id).
    ///   - counter unchanged + entity pre-existed → genuine RefExists (stale
    ///     counter / preexisting entity collision) → reject, no corruption.
    ///   - otherwise → a real git failure.
    pub fn allocate_id(&self) -> Result<u64, StoreError> {
        for _ in 0..MAX_ALLOC_RETRIES {
            match self.inner.current_tip(COUNTER_REF)? {
                None => {
                    let counter_oid = self.write_counter_commit(2, None)?;
                    let genesis = self.genesis_oid()?;
                    let lines = vec![
                        format!("update {COUNTER_REF} {counter_oid} {ZERO_OID}"),
                        format!("update {} {genesis} {ZERO_OID}", issue_ref(1)),
                    ];
                    match self.inner.run_update_ref_stdin(&lines) {
                        Ok(()) => return Ok(1),
                        Err(e) => {
                            // Counter first: if it appeared, a concurrent first
                            // allocator won → retry (fresh read gives id via the
                            // existing-counter path).
                            if self.inner.current_tip(COUNTER_REF)?.is_some() {
                                continue;
                            }
                            // Counter still absent: the batch failed for another
                            // reason — a pre-existing issue #1 is the stale
                            // collision case; otherwise a real git error.
                            if self.inner.current_tip(&issue_ref(1))?.is_some() {
                                return Err(StoreError::RefExists(issue_ref(1)));
                            }
                            return Err(e);
                        }
                    }
                }
                Some(counter_tip) => {
                    let next = self.inner.read_counter_next(counter_tip)?;
                    // Versioned chain: the new counter commit's sole parent is
                    // the observed counter tip (wire contract).
                    let counter_new = self.write_counter_commit(next + 1, Some(counter_tip))?;
                    let genesis = self.genesis_oid()?;
                    let lines = vec![
                        format!("update {COUNTER_REF} {counter_new} {counter_tip}"),
                        format!("update {} {genesis} {ZERO_OID}", issue_ref(next)),
                    ];
                    match self.inner.run_update_ref_stdin(&lines) {
                        Ok(()) => return Ok(next),
                        Err(e) => {
                            // Counter first: if it moved from counter_tip, a
                            // concurrent allocator won → retry with a fresh id.
                            if self.inner.current_tip(COUNTER_REF)? != Some(counter_tip) {
                                continue;
                            }
                            // Counter unchanged (whole batch rolled back): a
                            // pre-existing entity ref is the stale-collision
                            // case; otherwise a real git error.
                            if self.inner.current_tip(&issue_ref(next))?.is_some() {
                                return Err(StoreError::RefExists(issue_ref(next)));
                            }
                            return Err(e);
                        }
                    }
                }
            }
        }
        Err(StoreError::Exhausted)
    }

    /// Atomically create a PR. Parameter order is fixed and documented:
    /// `(title, source_ref, base_ref, source_oid, base_oid, merge_base,
    /// actor)` — title first (primary user input), then the two branch names,
    /// then the three OIDs (distinct `Oid` type; cannot swap with the `&str`s),
    /// then the event actor (`user.email`). The `pr.created` event commit
    /// (parent = genesis) is written first, then ONE `git update-ref --stdin`
    /// transaction CASes the counter and creates `/head` → event commit,
    /// `/meta` → same event commit (convenience pointer), `/source` →
    /// `source_oid`, `/base` → `base_oid`, all with expected absence. Any
    /// failure leaves counter and all four PR refs unchanged; a pre-existing
    /// PR ref for the target id is `RefExists`.
    ///
    /// Clippy `too-many-arguments` is suppressed: the positional order is a
    /// documented, test-pinned wire contract, and bundling the snapshot OIDs
    /// or the actor into a struct would churn the public store API for a lint
    /// threshold.
    #[allow(clippy::too_many_arguments)]
    pub fn create_pr(
        &self,
        title: &str,
        source_ref: &str,
        base_ref: &str,
        source_oid: Oid,
        base_oid: Oid,
        merge_base: Oid,
        actor: &str,
    ) -> Result<u64, StoreError> {
        for _ in 0..MAX_ALLOC_RETRIES {
            let (counter_tip, counter_new, next, genesis) =
                match self.inner.current_tip(COUNTER_REF)? {
                    None => (
                        None,
                        self.write_counter_commit(2, None)?,
                        1,
                        self.genesis_oid()?,
                    ),
                    Some(ct) => {
                        let next = self.inner.read_counter_next(ct)?;
                        (
                            Some(ct),
                            self.write_counter_commit(next + 1, Some(ct))?,
                            next,
                            self.genesis_oid()?,
                        )
                    }
                };
            let mut body = HashMap::new();
            body.insert("title".into(), JsonValue::String(title.to_string()));
            body.insert(
                "source_ref".into(),
                JsonValue::String(source_ref.to_string()),
            );
            body.insert("base_ref".into(), JsonValue::String(base_ref.to_string()));
            body.insert(
                "source_head".into(),
                JsonValue::String(source_oid.to_string()),
            );
            body.insert("base_head".into(), JsonValue::String(base_oid.to_string()));
            body.insert(
                "merge_base".into(),
                JsonValue::String(merge_base.to_string()),
            );
            let event = Event::new(EventKind::PrCreated, "pr", next, actor, body);
            let message = format!("forge:{}:{}", event.kind.as_str(), event.entity_id);
            let event_oid = self.write_event_commit(&event, &[genesis], &message)?;
            let plan = [
                (pr_head_ref(next), event_oid),
                (pr_meta_ref(next), event_oid),
                (pr_source_ref(next), source_oid),
                (pr_base_ref(next), base_oid),
            ];
            // Counter CAS line: expected old (or zeros to require absence).
            let old = counter_tip
                .map(|o| o.to_string())
                .unwrap_or_else(|| ZERO_OID.to_string());
            let mut lines = vec![format!("update {COUNTER_REF} {counter_new} {old}")];
            for (r, oid) in &plan {
                lines.push(format!("update {r} {oid} {ZERO_OID}"));
            }
            match self.inner.run_update_ref_stdin(&lines) {
                Ok(()) => return Ok(next),
                Err(e) => {
                    // Counter first: if it moved, a concurrent allocator won →
                    // retry with a fresh id.
                    if self.inner.current_tip(COUNTER_REF)? != counter_tip {
                        continue;
                    }
                    // Counter unchanged (batch rolled back): a pre-existing PR
                    // ref is the stale-collision case.
                    let mut any_preexisting = false;
                    for (r, _) in &plan {
                        if self.inner.current_tip(r)?.is_some() {
                            any_preexisting = true;
                            break;
                        }
                    }
                    if any_preexisting {
                        return Err(StoreError::RefExists(pr_head_ref(next)));
                    }
                    return Err(e);
                }
            }
        }
        Err(StoreError::Exhausted)
    }

    /// Atomically create the pending result ref `refs/forge/prs/<n>/result` →
    /// `result_commit` (expected absence). It keeps the freshly-built merge
    /// result reachable through `git gc` until the final completion
    /// transaction (wire contract § Merge completion).
    pub fn create_pending_result_ref(
        &self,
        pr_id: u64,
        result_commit: Oid,
    ) -> Result<(), StoreError> {
        let r = pr_result_ref(pr_id);
        self.inner
            .run_update_ref_stdin(&[format!("update {r} {result_commit} {ZERO_OID}")])
    }

    /// Best-effort deletion of the pending result ref (CAS: it must hold
    /// `result_commit`). Ok(true) deleted; Ok(false) ref was already absent
    /// (nothing to clean); Err when the ref still exists but CAS failed.
    pub fn delete_pending_result_ref(
        &self,
        pr_id: u64,
        result_commit: Oid,
    ) -> Result<bool, StoreError> {
        let r = pr_result_ref(pr_id);
        let line = format!("update {r} {ZERO_OID} {result_commit}");
        match self.inner.run_update_ref_stdin(&[line]) {
            Ok(()) => Ok(true),
            Err(_) => {
                if self.inner.current_tip(&r)?.is_none() {
                    Ok(false) // already gone
                } else {
                    Err(StoreError::RefExists(r))
                }
            }
        }
    }

    /// Write an unappended `pr.merge` event commit (dangling object, parent =
    /// current head tip) so the PR chain can be CAS-moved to it in the same
    /// transaction as the base branch and the pending-ref deletion.
    fn write_unappended_pr_merge_commit(
        &self,
        pr_id: u64,
        head_tip: Oid,
        result_commit: Oid,
        actor: &str,
    ) -> Result<Oid, StoreError> {
        let mut body = HashMap::new();
        body.insert(
            "result_commit".into(),
            JsonValue::String(result_commit.to_string()),
        );
        let event = Event::new(EventKind::PrMerge, "pr", pr_id, actor, body);
        let message = format!("forge:{}:{}", event.kind.as_str(), event.entity_id);
        self.write_event_commit(&event, &[head_tip], &message)
    }

    /// Atomic merge completion (wire contract § Merge completion): write the
    /// `pr.merge` event commit (dangling), then ONE `git update-ref --stdin`
    /// transaction that (1) deletes the pending `/result` ref, (2) CAS-updates
    /// `refs/heads/<base_ref>` from the PR's snapshot `base_head` to
    /// `result_commit`, and (3) CAS-moves the PR `/head` chain to the
    /// `pr.merge` event commit. The head chain tip is read here, and the
    /// `pr.merge` event commit parented to it, both inside the same function
    /// as the transaction — so head, base, and the pending-ref deletion move
    /// as one atomic unit. On failure nothing moved; the caller reports the
    /// leftover pending ref. `actor` is the invoking repo's `user.email`
    /// (wire contract `"actor": "<user.email>"`).
    pub fn finalize_pr_merge(
        &self,
        pr_id: u64,
        base_ref: &str,
        base_expected: Oid,
        result_commit: Oid,
        actor: &str,
    ) -> Result<(), StoreError> {
        let head_ref = pr_head_ref(pr_id);
        let head_tip = self
            .inner
            .current_tip(&head_ref)?
            .ok_or(StoreError::MissingRef)?;
        let head_new =
            self.write_unappended_pr_merge_commit(pr_id, head_tip, result_commit, actor)?;
        let result = pr_result_ref(pr_id);
        let base_branch_ref = format!("refs/heads/{base_ref}");
        let lines = vec![
            // delete pending result ref (expected presence).
            format!("update {result} {ZERO_OID} {result_commit}"),
            // CAS base branch from snapshot base_head to result_commit.
            format!("update {base_branch_ref} {result_commit} {base_expected}"),
            // CAS PR head chain from old tip to pr.merge commit.
            format!("update {head_ref} {head_new} {head_tip}"),
        ];
        self.inner.run_update_ref_stdin(&lines)
    }
}

/// Parse `{"v":1,"next":<u64>}` (tolerates whitespace).
fn parse_counter_next(content: &str) -> Result<u64, StoreError> {
    let trimmed = content.trim();
    let key = "\"next\":";
    let idx = trimmed
        .find(key)
        .ok_or_else(|| StoreError::InvalidState("counter.json missing next".into()))?;
    let rest = &trimmed[idx + key.len()..];
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits
        .parse::<u64>()
        .map_err(|_| StoreError::InvalidState("counter next not a u64".into()))
}
