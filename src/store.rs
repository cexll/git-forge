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
mod issue_batch;
pub(crate) use issue_batch::{expected_value_rejected, IssueSnapshot};

/// The single mutable counter ref holding the next sequential id.
pub const COUNTER_REF: &str = "refs/forge/meta/counter";

/// Genesis commit message for empty entity-chain roots (no event payload).
const GENESIS_MSG: &str = "forge:genesis";

/// Number of bounded CAS append retries (wire contract default).
pub(crate) const MAX_APPEND_RETRIES: u32 = 3;

/// Number of bounded allocation retries on counter collision.
pub(crate) const MAX_ALLOC_RETRIES: u32 = 3;

/// All-zero OID used by `git update-ref` to mean "ref must not exist".
pub(crate) const ZERO_OID: &str = "0000000000000000000000000000000000000000";

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

/// Generation-addressable snapshot refs (t1 pr.update): generation k >= 1
/// pins its source/base snapshot immutably under `gen/<k>/source` and
/// `gen/<k>/base`. Generation 0 (the authoritative `pr.created` snapshot)
/// keeps the plain `/source` and `/base` refs, which stay immutable forever.
/// The `gen/<k>/` layout is not the ticket's illustrative `/source/g<k>`:
/// the files ref backend refuses a D/F sibling pair (`/source` exists, so
/// `/source/g1` cannot be created), while `gen/<k>/` shares no prefix with
/// any existing per-PR ref.
pub fn pr_source_generation_ref(n: u64, generation: u64) -> String {
    format!("refs/forge/prs/{n}/gen/{generation}/source")
}
pub fn pr_base_generation_ref(n: u64, generation: u64) -> String {
    format!("refs/forge/prs/{n}/gen/{generation}/base")
}

/// The ref that pins generation `generation`'s source snapshot: the L1
/// `/source` ref for generation 0, the generation ref for k >= 1.
pub fn pr_source_ref_for(n: u64, generation: u64) -> String {
    if generation == 0 {
        pr_source_ref(n)
    } else {
        pr_source_generation_ref(n, generation)
    }
}

/// The ref that pins generation `generation`'s base snapshot.
pub fn pr_base_ref_for(n: u64, generation: u64) -> String {
    if generation == 0 {
        pr_base_ref(n)
    } else {
        pr_base_generation_ref(n, generation)
    }
}

pub fn pr_result_ref(n: u64) -> String {
    format!("refs/forge/prs/{n}/result")
}

/// Store errors. `CasConflict`/`Exhausted` are the retry-signalling failures;
/// `RefExists` guards entity creation collisions.
#[derive(Debug)]
pub enum StoreError {
    Git(GitError),
    /// The current directory is not inside a git repository (or any parent
    /// directory). Raised by `EventStore::open`/`init` when `Repository::open`
    /// reports `ErrorCode::NotFound`, so the CLI shows a user-facing message
    /// instead of libgit2's internal error string.
    NotAGitRepository,
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
            StoreError::NotAGitRepository => {
                write!(f, "not a git repository (or any of the parent directories)")
            }
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

/// Map a git2 `Repository::open`/`init` failure: a not-found current directory
/// (not inside any git repository) becomes the user-facing
/// [`StoreError::NotAGitRepository`] instead of leaking libgit2's internal
/// error string. Every other git2 error stays a `StoreError::Git`.
fn map_repo_open_err(e: GitError) -> StoreError {
    if e.code() == git2::ErrorCode::NotFound {
        StoreError::NotAGitRepository
    } else {
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
        let repo = Repository::open(path.as_ref()).map_err(map_repo_open_err)?;
        Self::from_repo(repo)
    }

    pub fn init(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        let repo = Repository::init(path.as_ref()).map_err(map_repo_open_err)?;
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
    pub(crate) fn run_update_ref_stdin(&self, lines: &[String]) -> Result<(), StoreError> {
        use std::io::Write;
        let mut child = Command::new("git")
            .arg("--git-dir")
            .arg(&self.git_dir)
            .arg("update-ref")
            .arg("--stdin")
            .env("LC_ALL", "C")
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

    pub(crate) fn current_tip(&self, entity_ref: &str) -> Result<Option<Oid>, StoreError> {
        match self.repo.find_reference(entity_ref) {
            Ok(r) => Ok(r.target()),
            Err(e) if e.code() == git2::ErrorCode::NotFound => Ok(None),
            Err(e) => Err(StoreError::Git(e)),
        }
    }

    /// Walk a chain oldest→tip from a known tip OID, returning each
    /// event-bearing commit's OID paired with its event. Skips commits whose
    /// tree has no `.forge/event.json` (genesis roots and L2 merge nodes).
    /// THE first-parent chain walk. One traversal primitive, shared by the
    /// OID-bearing [`Self::chain_from_tip`] and the event-only
    /// [`Self::chain_events_from_tip`]: walks first-parent commits from `tip`,
    /// projects each event-bearing commit with `build`, and returns oldest→tip.
    fn walk_first_parent<T>(
        &self,
        tip: Oid,
        mut build: impl FnMut(Oid, Event) -> T,
    ) -> Result<Vec<T>, StoreError> {
        let mut out: Vec<T> = Vec::new();
        let mut oid = tip;
        loop {
            let commit = self.repo.find_commit(oid)?;
            if let Some(event) = self.read_event_blob(&commit)? {
                out.push(build(oid, event));
            }
            match commit.parent_ids().next() {
                Some(p) => oid = p,
                None => break,
            }
        }
        out.reverse();
        Ok(out)
    }

    /// Event-bearing commit's OID paired with its event, oldest→tip. Skips
    /// commits whose tree has no `.forge/event.json` (genesis roots and L2
    /// merge nodes).
    fn chain_from_tip(&self, tip: Oid) -> Result<Vec<(Oid, Event)>, StoreError> {
        self.walk_first_parent(tip, |oid, ev| (oid, ev))
    }

    /// Event chain oldest→tip as events, directly (no intermediate
    /// OID/event Vec). Skips commits with no event blob.
    fn chain_events_from_tip(&self, tip: Oid) -> Result<Vec<Event>, StoreError> {
        self.walk_first_parent(tip, |_, ev| ev)
    }

    /// Read a single entity event chain oldest→tip. Skips commits whose tree
    /// has no `.forge/event.json` (genesis roots and L2 merge nodes).
    pub fn read_chain(&self, entity_ref: &str) -> Result<Vec<Event>, StoreError> {
        let Some(tip) = self.current_tip(entity_ref)? else {
            return Ok(Vec::new());
        };
        self.chain_events_from_tip(tip)
    }

    /// Read an event chain oldest→`tip`, where `tip` is an event commit (for
    /// example the one `append_event` returned), ignoring later appends.
    pub fn read_chain_at(&self, tip: Oid) -> Result<Vec<Event>, StoreError> {
        self.chain_events_from_tip(tip)
    }

    /// Pure comparison of the snapshot refs against an AUTHORITATIVE
    /// `pr.created` state (the caller MUST have obtained it from an anchored
    /// read). The merge gate passes its ALREADY-ANCHORED gate state here, so a
    /// concurrent `/head` rewrite between the gate read and the finalize cannot
    /// substitute a forged snapshot (F-008).
    pub(crate) fn validate_snapshot_against_state(
        id: u64,
        state: &crate::event::PrState,
        source_oid: Oid,
        base_oid: Oid,
    ) -> Result<(), String> {
        let auth_source = state
            .source_head
            .as_deref()
            .map(|s| s.parse::<Oid>())
            .transpose()
            .map_err(|_| "PR source_head invalid".to_string())?
            .ok_or_else(|| "PR has no source_head".to_string())?;
        let auth_base = state
            .base_head
            .as_deref()
            .map(|s| s.parse::<Oid>())
            .transpose()
            .map_err(|_| "PR base_head invalid".to_string())?
            .ok_or_else(|| "PR has no base_head".to_string())?;
        if source_oid != auth_source || base_oid != auth_base {
            return Err(format!(
                "PR #{id} snapshot refs were retargeted (source {source_oid} != {auth_source} \
                 or base {base_oid} != {auth_base}); recreate the PR"
            ));
        }
        Ok(())
    }

    /// True iff the WHOLE chain folded at `tip` is anchored to PR `id` (F-008):
    /// every event-bearing commit along the first-parent chain carries
    /// `entity == "pr"` with `entity_id == id`, and the chain contains PR
    /// `id`'s authoritative `pr.created` event — the exact commit pinned by the
    /// immutable `/meta` ref — at the chain's anchor position. The previous
    /// `fold(...).pr.id == id` check was unsound because `fold` overwrites
    /// `pr.id` for every PR event, and the earlier `saw_created` check accepted
    /// ANY `pr.created` event with matching entity/id, so a synthetic same-id
    /// chain carrying a forged (non-`/meta`) `pr.created` could fold a forged
    /// approval + green CI Check. A missing `/meta` ref, a missing /
    /// replacement / duplicate `pr.created`, or a cross-entity / mixed chain all
    /// refuse.
    /// Anchor scan shared by [`Self::pr_chain_anchored_to`] and
    /// [`Self::read_pr_chain_anchored`]: every event must belong to entity
    /// `pr`/`entity_id` id, and exactly one `pr.created` must be present at the
    /// authoritative `/meta` commit.
    fn chain_scan_pr_anchored(&self, id: u64, meta_oid: Oid, pairs: &[(Oid, Event)]) -> bool {
        if pairs.is_empty() {
            return false;
        }
        let mut created = 0usize;
        for (oid, ev) in pairs {
            if ev.entity != "pr" || ev.entity_id != id {
                return false;
            }
            if ev.kind == EventKind::PrCreated {
                if *oid != meta_oid {
                    return false;
                }
                created += 1;
            }
        }
        created == 1
    }

    pub(crate) fn pr_chain_anchored_to(&self, id: u64, tip: Oid) -> bool {
        // The authoritative creation commit: `/meta` is pinned at PR allocation
        // to the exact `pr.created` snapshot commit and never moves (F-006,
        // F-008). If it is absent the chain cannot be verified as PR `id`'s.
        let Ok(meta) = self.current_tip(&pr_meta_ref(id)) else {
            return false;
        };
        let Some(meta_oid) = meta else {
            return false;
        };
        let Ok(chain) = self.chain_from_tip(tip) else {
            return false;
        };
        self.chain_scan_pr_anchored(id, meta_oid, &chain)
    }

    /// Read the chain at `tip` AND validate it is anchored to PR `id` in a
    /// SINGLE chain walk (F-008): the merge gate folds the returned events
    /// directly instead of re-walking the chain a second time. Returns
    /// `(true, events)` when anchored, `(false, Vec::new())` otherwise.
    pub(crate) fn read_pr_chain_anchored(
        &self,
        id: u64,
        tip: Oid,
    ) -> Result<(bool, Vec<Event>), StoreError> {
        // Propagate a REAL git error (corrupt object, I/O) so the merge gate
        // surfaces it as a git failure, not as a misleading "not anchored to
        // PR #id" — only a genuinely-absent `/meta` (Ok(None)) is "not
        // anchored".
        let meta = self.current_tip(&pr_meta_ref(id))?;
        let Some(meta_oid) = meta else {
            return Ok((false, Vec::new()));
        };
        let pairs = self.chain_from_tip(tip)?;
        let anchored = self.chain_scan_pr_anchored(id, meta_oid, &pairs);
        let events = pairs.into_iter().map(|(_, ev)| ev).collect();
        Ok((anchored, events))
    }

    /// True when `tip` is a valid first-parent extension of `ancestor` on PR
    /// `id`'s chain (F-008). A legitimate concurrent append moves the PR head
    /// past the gate-validated tip; a rewrite, a cross-PR / cross-entity tip,
    /// or any other non-append move does not. Walking the first-parent chain
    /// from `tip` must reach `ancestor` exactly, and the whole chain folded at
    /// `tip` must be anchored to PR `id` (via [`Self::pr_chain_anchored_to`],
    /// which also rejects a mixed/foreign masquerade). Returns false for a
    /// non-append move, so the merge finalizer refuses with refs unchanged and
    /// the pending result ref left in place rather than silently retrying into
    /// a foreign chain.
    pub fn head_extends_pr_chain(&self, id: u64, ancestor: Oid, tip: Oid) -> bool {
        if tip == ancestor {
            return false;
        }
        // First-parent reach: `tip` must descend from `ancestor` (a legitimate
        // concurrent append), never a rewrite or a cross-PR tip.
        let mut cur = tip;
        loop {
            let commit = match self.repo.find_commit(cur) {
                Ok(c) => c,
                Err(_) => return false,
            };
            if cur == ancestor {
                break;
            }
            match commit.parent_ids().next() {
                Some(p) => cur = p,
                None => return false,
            }
        }
        // Chain-anchor validation: the whole chain at `tip` is PR `id`'s —
        // anchored by PR #id's authoritative `pr.created` event and every
        // event-bearing commit belongs to entity pr / entity_id id.
        self.pr_chain_anchored_to(id, tip)
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
    /// Read an event chain as of one event commit (delegation to the read surface).
    pub fn read_chain_at(&self, tip: Oid) -> Result<Vec<Event>, StoreError> {
        self.inner.read_chain_at(tip)
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

    pub(crate) fn write_event_commit(
        &self,
        event: &Event,
        parents: &[Oid],
        message: &str,
    ) -> Result<Oid, StoreError> {
        // t8 (#8): serialize through the identity layer, which stamps the
        // command's resolved role onto the wire JSON when one is configured
        // (no role configured → `Event::to_json()` verbatim, byte-identical
        // to pre-t8). One stamping point for every event write path.
        let wire = crate::identity::stamped_event_json(event);
        self.write_forge_commit("event.json", wire.as_bytes(), parents, message)
    }

    /// Write the initial pending CI Check `ci.check` child for a freshly
    /// created PR, parented on the `pr.created` commit (F-006). It carries only
    /// `status: "pending"` — no plan executes at creation time — and becomes
    /// the `/head` chain tip so the fold surfaces the newest CI outcome.
    fn write_pending_ci_check_commit(
        &self,
        pr_id: u64,
        created_oid: Oid,
        actor: &str,
    ) -> Result<Oid, StoreError> {
        let mut body = HashMap::new();
        body.insert("status".into(), JsonValue::String("pending".to_string()));
        let event = Event::new(EventKind::CiCheck, "pr", pr_id, actor, body);
        let message = format!("forge:{}:{}", event.kind.as_str(), event.entity_id);
        self.write_event_commit(&event, &[created_oid], &message)
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
    /// actor, description, labels)` — title first (primary user input), then
    /// the two branch names, then the three OIDs (distinct `Oid` type; cannot
    /// swap with the `&str`s), then the event actor (`user.email`), then the
    /// optional PR description and the label list (both None/empty when
    /// absent). The `pr.created` event commit
    /// (parent = genesis) is written first, followed by its pending `ci.check`
    /// child (parent = `pr.created`), then ONE `git update-ref --stdin`
    /// transaction CASes the counter and creates `/head` → the `ci.check`
    /// commit, `/meta` → the `pr.created` commit (convenience pointer),
    /// `/source` → `source_oid`, `/base` → `base_oid`, all with expected
    /// absence. Any failure leaves counter and all four PR refs unchanged (the
    /// pending CI Check marker is published atomically with PR allocation, so a
    /// failed `pr create` cannot strand a durable PR without it — F-006); a
    /// pre-existing PR ref for the target id is `RefExists`.
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
        description: Option<&str>,
        labels: &[String],
        closes: Option<u64>,
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
            if let Some(d) = description {
                body.insert("description".into(), JsonValue::String(d.to_string()));
            }
            if !labels.is_empty() {
                body.insert("labels".into(), crate::event::json_string_array(labels));
            }
            if let Some(c) = closes {
                body.insert("closes".into(), JsonValue::Number(c as i64));
            }
            let event = Event::new(EventKind::PrCreated, "pr", next, actor, body);
            let message = format!("forge:{}:{}", event.kind.as_str(), event.entity_id);
            let event_oid = self.write_event_commit(&event, &[genesis], &message)?;
            // Publish the initial pending CI Check marker in the SAME atomic
            // transaction as PR allocation: /head points at the ci.check child,
            // /meta stays pinned at the pr.created snapshot commit (F-006).
            let ci_oid = self.write_pending_ci_check_commit(next, event_oid, actor)?;
            let plan = [
                (pr_head_ref(next), ci_oid),
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

    /// Re-pin PR `id` to `new_source` as a NEW immutable snapshot generation
    /// (t1 pr.update). Writes the `pr.update` event commit (sole parent = the
    /// observed `/head` tip, body: old/new source_head, old/new base_head,
    /// old/new merge_base) and creates the generation refs
    /// `/source/g<generation>` and `/base/g<generation>` → the new snapshot
    /// OIDs, then ONE `git update-ref --stdin` transaction CASes `/head`
    /// from the observed tip to the event commit. All-or-nothing: a failure
    /// leaves `/head` and both generation refs unchanged.
    ///
    /// Retry (bounded, like `append_event`): on CAS conflict the SAME event
    /// (retained UUID) is re-parented onto the fresh tip. The generation
    /// refs are created with expected absence; if a concurrent update
    /// already claimed this generation number, `RefExists` refuses — the
    /// caller recomputes the generation from the fresh fold.
    ///
    /// Parameter order is fixed and documented like `create_pr`: id, then
    /// the three old snapshot OIDs, then the three new ones, then the
    /// generation number, then the actor.
    #[allow(clippy::too_many_arguments)]
    pub fn update_pr(
        &self,
        id: u64,
        old_source: Oid,
        old_base: Oid,
        old_merge_base: Oid,
        new_source: Oid,
        new_base: Oid,
        new_merge_base: Oid,
        generation: u64,
        actor: &str,
    ) -> Result<Oid, StoreError> {
        let head = pr_head_ref(id);
        let source_gen_ref = pr_source_generation_ref(id, generation);
        let base_gen_ref = pr_base_generation_ref(id, generation);
        let mut body = HashMap::new();
        body.insert(
            "old_source_head".into(),
            JsonValue::String(old_source.to_string()),
        );
        body.insert(
            "new_source_head".into(),
            JsonValue::String(new_source.to_string()),
        );
        body.insert(
            "old_base_head".into(),
            JsonValue::String(old_base.to_string()),
        );
        body.insert(
            "new_base_head".into(),
            JsonValue::String(new_base.to_string()),
        );
        body.insert(
            "old_merge_base".into(),
            JsonValue::String(old_merge_base.to_string()),
        );
        body.insert(
            "new_merge_base".into(),
            JsonValue::String(new_merge_base.to_string()),
        );
        body.insert("generation".into(), JsonValue::Number(generation as i64));
        let event = Event::new(EventKind::PrUpdate, "pr", id, actor, body);
        let message = format!("forge:{}:{}", event.kind.as_str(), event.entity_id);
        let mut tip = self.inner.current_tip(&head)?;
        for _ in 0..MAX_APPEND_RETRIES {
            let parents: Vec<Oid> = tip.into_iter().collect();
            let event_oid = self.write_event_commit(&event, &parents, &message)?;
            let mut lines = vec![
                format!("update {source_gen_ref} {new_source} {ZERO_OID}"),
                format!("update {base_gen_ref} {new_base} {ZERO_OID}"),
            ];
            let old = tip
                .map(|o| o.to_string())
                .unwrap_or_else(|| ZERO_OID.to_string());
            lines.push(format!("update {head} {event_oid} {old}"));
            match self.inner.run_update_ref_stdin(&lines) {
                Ok(()) => return Ok(event_oid),
                Err(e) => {
                    // A generation ref existing after a failed batch means a
                    // concurrent update claimed this generation number: the
                    // caller must re-fold and use the next one.
                    if self.inner.current_tip(&source_gen_ref)?.is_some() {
                        return Err(StoreError::RefExists(source_gen_ref));
                    }
                    let fresh = self.inner.current_tip(&head)?;
                    if fresh == tip {
                        return Err(e);
                    }
                    tip = fresh;
                }
            }
        }
        Err(StoreError::Exhausted)
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

#[cfg(test)]
mod tests;
