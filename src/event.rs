use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JsonValue {
    Null,
    Bool(bool),
    Number(i64),
    String(String),
    Array(Vec<JsonValue>),
    Object(HashMap<String, JsonValue>),
}

impl JsonValue {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            JsonValue::String(s) => Some(s),
            _ => None,
        }
    }
    pub fn as_u64(&self) -> Option<u64> {
        match self {
            JsonValue::Number(n) if *n >= 0 => Some(*n as u64),
            _ => None,
        }
    }
    pub fn as_object(&self) -> Option<&HashMap<String, JsonValue>> {
        match self {
            JsonValue::Object(m) => Some(m),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventKind {
    IssueCreated,
    IssueUpdated,
    IssueComment,
    IssueClose,
    IssueReopen,
    PrCreated,
    PrUpdate,
    PrPrepared,
    PrComment,
    PrReview,
    PrMerge,
    PrAbandoned,
    CiCheck,
}

impl EventKind {
    pub fn as_str(self) -> &'static str {
        match self {
            EventKind::IssueCreated => "issue.created",
            EventKind::IssueUpdated => "issue.updated",
            EventKind::IssueComment => "issue.comment",
            EventKind::IssueClose => "issue.close",
            EventKind::IssueReopen => "issue.reopen",
            EventKind::PrCreated => "pr.created",
            EventKind::PrUpdate => "pr.update",
            EventKind::PrPrepared => "pr.prepared",
            EventKind::PrComment => "pr.comment",
            EventKind::PrReview => "pr.review",
            EventKind::PrMerge => "pr.merge",
            EventKind::PrAbandoned => "pr.abandoned",
            EventKind::CiCheck => "ci.check",
        }
    }
}

impl std::str::FromStr for EventKind {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "issue.created" => Self::IssueCreated,
            "issue.updated" => Self::IssueUpdated,
            "issue.comment" => Self::IssueComment,
            "issue.close" => Self::IssueClose,
            "issue.reopen" => Self::IssueReopen,
            "pr.created" => Self::PrCreated,
            "pr.update" => Self::PrUpdate,
            "pr.prepared" => Self::PrPrepared,
            "pr.comment" => Self::PrComment,
            "pr.review" => Self::PrReview,
            "pr.merge" => Self::PrMerge,
            "pr.abandoned" => Self::PrAbandoned,
            "ci.check" => Self::CiCheck,
            _ => return Err(()),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    pub v: u32,
    pub id: String,
    pub kind: EventKind,
    pub entity: String,
    pub entity_id: u64,
    pub ts: String,
    pub actor: String,
    pub body: HashMap<String, JsonValue>,
}

/// Validates RFC 4122 version-4 UUID textual shape: 8-4-4-4-12 hex groups,
/// version nibble 4, variant nibble in [89ab].
pub fn is_uuid_v4(input: &str) -> bool {
    let bytes = input.as_bytes();
    if bytes.len() != 36 {
        return false;
    }
    for (i, b) in bytes.iter().enumerate() {
        match (i, b) {
            (8, _) | (13, _) | (18, _) | (23, _) => {
                if *b != b'-' {
                    return false;
                }
            }
            (_, b) if *b == b'-' => return false,
            (_, b) if b.is_ascii_hexdigit() => {}
            _ => return false,
        }
    }
    bytes[14] == b'4' && matches!(bytes[19], b'8' | b'9' | b'a' | b'b')
}

fn uuid_v4_from_seed(seed: u64) -> String {
    // Deterministic std-only UUID-shaped id. Not cryptographically random:
    // callers that require provenance must pass their own validated UUID.
    let mut state = seed | 1;
    let mut bytes = [0u8; 16];
    for b in bytes.iter_mut() {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        *b = state as u8;
    }
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0], bytes[1], bytes[2], bytes[3],
        bytes[4], bytes[5],
        bytes[6], bytes[7],
        bytes[8], bytes[9],
        bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15]
    )
}

/// Current system time as an RFC3339 UTC timestamp, seconds granularity
/// (`YYYY-MM-DDTHH:MM:SSZ`). The v1 event schema contract is `"ts":
/// "<RFC3339 UTC>"`; a std-only epoch→civil conversion keeps the schema
/// dependency-free (no chrono).
fn rfc3339_utc_now() -> String {
    format_rfc3339_system_time(SystemTime::now())
}

/// Format an arbitrary `SystemTime` as RFC3339 UTC seconds. A clock BEFORE the
/// Unix epoch (reset/misconfigured wall clock) must format a real pre-1970
/// time, never the fabricated `1970-01-01T00:00:00Z` (F-001).
// F-001 (USER-ACCEPTED KNOWN LIMITATION): a wall clock in the subsecond
// window just before the Unix epoch (a pre-1970 clock, itself already a
// fault) truncates to 0 whole seconds and formats to `1970-01-01T00:00:00Z`
// here. That is accepted: this formatter is seconds-granularity and ordering
// semantics come from parent-chain position, not from `ts`. No logic change.
fn format_rfc3339_system_time(t: SystemTime) -> String {
    match t.duration_since(UNIX_EPOCH) {
        Ok(d) => format_rfc3339_utc(d.as_secs() as i64),
        Err(e) => format_rfc3339_utc(-(e.duration().as_secs() as i64)),
    }
}

/// Format signed seconds-since-epoch as RFC3339 UTC `YYYY-MM-DDTHH:MM:SSZ`.
/// Negative seconds (a wall clock before the Unix epoch) format to a real
/// pre-1970 civil time via floor/euclid division, never a fabricated
/// `1970-01-01T00:00:00Z` (F-001). The `civil_from_days` inverse already
/// accepts signed day counts, so the same proleptic-Gregorian conversion
/// produces the correct pre-epoch date.
fn format_rfc3339_utc(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let secs_of_day = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    let h = secs_of_day / 3600;
    let min = (secs_of_day % 3600) / 60;
    let s = secs_of_day % 60;
    format!("{y:04}-{m:02}-{d:02}T{h:02}:{min:02}:{s:02}Z")
}

/// Days-since-1970-01-01 → proleptic-Gregorian `(year, month, day)`, the
/// inverse of the standard days-from-civil algorithm (Howard Hinnant).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

impl Event {
    /// Creates an event with a UUID-v4-shaped id from a std-only seed.
    /// The seed is not a cryptographic entropy source; production id
    /// provenance belongs to the store layer (uuid crate) in later slices.
    pub fn new(
        kind: EventKind,
        entity: &str,
        entity_id: u64,
        actor: &str,
        body: HashMap<String, JsonValue>,
    ) -> Self {
        let seed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0)
            ^ entity_id;
        Self {
            v: 1,
            id: uuid_v4_from_seed(seed),
            kind,
            entity: entity.to_string(),
            entity_id,
            ts: rfc3339_utc_now(),
            actor: actor.to_string(),
            body,
        }
    }

    /// Constructs an event with caller-supplied provenance. Rejects non-UUID-v4
    /// ids; the caller owns cryptographic provenance, the schema owner owns shape.
    pub fn new_with_id(
        id: &str,
        kind: EventKind,
        entity: &str,
        entity_id: u64,
        actor: &str,
        body: HashMap<String, JsonValue>,
    ) -> Option<Self> {
        if !is_uuid_v4(id) {
            return None;
        }
        Some(Self {
            v: 1,
            id: id.to_string(),
            kind,
            entity: entity.to_string(),
            entity_id,
            ts: rfc3339_utc_now(),
            actor: actor.to_string(),
            body,
        })
    }

    pub fn from_json(input: &str) -> Option<Self> {
        let root = parse_json_value(input)?;
        let object = root.as_object()?;
        let v = object.get("v")?.as_u64()?;
        if v != 1 {
            return None;
        }
        let id = object.get("id")?.as_str()?.to_string();
        if !is_uuid_v4(&id) {
            return None;
        }
        let kind = object.get("kind")?.as_str()?.parse().ok()?;
        let entity = object.get("entity")?.as_str()?.to_string();
        let entity_id = object.get("entity_id")?.as_u64()?;
        let ts = object.get("ts")?.as_str()?.to_string();
        let actor = object.get("actor")?.as_str()?.to_string();
        let body = match object.get("body")? {
            JsonValue::Object(m) => m.clone(),
            _ => return None,
        };
        Some(Self {
            v: v as u32,
            id,
            kind,
            entity,
            entity_id,
            ts,
            actor,
            body,
        })
    }

    pub fn to_json(&self) -> String {
        format!(
            "{{\"v\":{},\"id\":{},\"kind\":{},\"entity\":{},\"entity_id\":{},\"ts\":{},\"actor\":{},\"body\":{}}}",
            self.v,
            json_string(&self.id),
            json_string(self.kind.as_str()),
            json_string(&self.entity),
            self.entity_id,
            json_string(&self.ts),
            json_string(&self.actor),
            json_object(&self.body)
        )
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IssueState {
    pub id: u64,
    pub title: Option<String>,
    pub description: Option<String>,
    pub key: Option<String>,
    pub parent: Option<u64>,
    pub blocked_by: Vec<u64>,
    pub labels: Vec<String>,
    pub comments: Vec<String>,
    pub open: bool,
}

/// A materialized merge candidate produced by `pr prepare <n> --strategy <s>`.
///
/// The candidate is the immutable result the PR pins (`refs/forge/prs/<n>/result`)
/// until merge finalization consumes it. Every later gate input (`ci run`,
/// `review`) and the merge finalize CAS are required to bind this exact
/// candidate: `prepared_base_head` is the base-branch tip the candidate was
/// built against and `prepared_generation` the snapshot generation it was
/// built from, so a base move or a source `pr.update` visibly invalidates it
/// (merge refuses with a re-prepare remedy) instead of silently re-deriving
/// the result.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PreparedCandidate {
    /// Snapshot generation the candidate was prepared from.
    pub generation: u64,
    /// Snapshot source commit the candidate was prepared from.
    pub source_head: String,
    /// Base-branch tip the candidate was built against (may differ from the
    /// snapshot base_head when the base moved — the live-base case).
    pub base_head: String,
    /// Strategy (`merge`, `squash`, `rebase`, `ff`) the candidate materializes.
    pub strategy: String,
    /// The materialized result commit (reachable: merge finalize CASes the
    /// base branch from `base_head` to this commit).
    pub result_commit: String,
    /// The result commit's tree — identity of the result content CI validates.
    pub result_tree: String,
    /// Recompute `merge_base(source_head, base_head)` at prepare time.
    pub merge_base: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PrState {
    pub id: u64,
    pub title: Option<String>,
    pub description: Option<String>,
    pub labels: Vec<String>,
    pub source_ref: Option<String>,
    pub base_ref: Option<String>,
    pub source_head: Option<String>,
    pub base_head: Option<String>,
    pub merge_base: Option<String>,
    /// Issue id the PR declares it closes (from the optional `closes` body
    /// field of `pr.created`); `None` when the PR declares no relationship.
    pub closes: Option<u64>,
    pub comments: Vec<String>,
    pub effective_decision: Option<String>,
    pub merge_result: Option<String>,
    /// Abandon reason from the latest `pr.abandoned` event (`None` = live PR).
    /// An abandoned PR is terminal: merge/review/ci run refuse it, and no
    /// reopen exists — a reopened PR is a new entity.
    pub abandoned: Option<String>,
    /// Latest CI Check status (success/failed/pending) — the fold keeps the
    /// most recently appended `ci.check` event, so the merge gate can read a
    /// single latest status. `None` when no CI Check has been recorded yet.
    pub ci_status: Option<String>,
    /// The plan (`.forge/ci.sh` or `just check`) that produced the latest
    /// CI Check.
    pub ci_plan: Option<String>,
    /// Actor of the latest CI Check.
    pub ci_actor: Option<String>,
    /// Current snapshot generation: 0 for the authoritative `pr.created`
    /// snapshot, +1 per `pr.update` event. Names which source ref pins the
    /// PR's current snapshot (`/source` for 0, `/source/g<k>` for k >= 1).
    pub generation: u64,
    /// Snapshot generation the current effective review decision was made
    /// against. A decision from an older generation cannot satisfy the merge
    /// gate (pre-update approvals are stale).
    pub decision_generation: u64,
    /// Snapshot generation the latest CI Check ran against. A Check from an
    /// older generation cannot satisfy the merge gate (pre-update Checks are
    /// stale).
    pub ci_generation: u64,
    /// Live base-branch tip the current effective review decision was made
    /// against (#2 result-bound review): `pr.review` records the base branch
    /// tip at review time, so the live-base merge gate can require a decision
    /// bound to the CURRENT base tip when the base has moved since the PR
    /// snapshot. Legacy events without the field fold as `None`.
    pub decision_base_head: Option<String>,
    /// Live base-branch tip the latest CI Check was taken against (#2
    /// result-bound CI): `ci.check` records the base branch tip at run time
    /// for the same live-base gate requirement. Legacy events fold as
    /// `None`.
    pub ci_base_head: Option<String>,
    /// Timestamp of the latest CI Check.
    pub ci_ts: Option<String>,
    /// The latest `pr.prepared` candidate (last-wins), if any. `None` when no
    /// prepare has run (unmoved-base legacy path). Merge of a moved base
    /// requires a candidate; the gate cross-checks its generation and
    /// base_head against the current PR/base state so staleness is refused
    /// rather than re-derived.
    pub prepared: Option<PreparedCandidate>,
    /// Strategy the current effective review decision binds (from the
    /// `strategy` field of `pr.review` recorded while a candidate exists);
    /// `None` for legacy decisions.
    pub decision_strategy: Option<String>,
    /// Candidate result commit the current effective review decision binds;
    /// `None` for legacy decisions.
    pub decision_result_commit: Option<String>,
    /// Candidate result tree the current effective review decision binds;
    /// `None` for legacy decisions.
    pub decision_result_tree: Option<String>,
    /// Strategy the latest CI Check binds (from `ci.check` run against a
    /// candidate); `None` for legacy source-snapshot Checks.
    pub ci_strategy: Option<String>,
    /// Candidate result commit the latest CI Check ran against; `None` for
    /// legacy source-snapshot Checks.
    pub ci_result_commit: Option<String>,
    /// Candidate result tree the latest CI Check ran against; `None` for
    /// legacy source-snapshot Checks.
    pub ci_result_tree: Option<String>,
    /// Role of the `pr.created` actor (#8, t8): the `role` body field the
    /// creating command stamped (`worker`/`reviewer`/`orchestrator`), `None`
    /// when no role was configured — the unchanged single-user default.
    pub creator_role: Option<String>,
    /// Role of the current effective review decision's actor (#8): the
    /// `role` body field of the last reachable `pr.review`. `None` for a
    /// roleless decision (default) or no decision yet.
    pub decision_role: Option<String>,
}

fn str_field(body: &HashMap<String, JsonValue>, key: &str) -> Option<String> {
    body.get(key).and_then(JsonValue::as_str).map(String::from)
}

/// The `generation` body field, or `default` for events written without it.
fn generation_field(body: &HashMap<String, JsonValue>, default: u64) -> u64 {
    body.get("generation")
        .and_then(JsonValue::as_u64)
        .unwrap_or(default)
}

impl PrState {
    fn apply(&mut self, event: &Event) {
        match event.kind {
            EventKind::PrCreated => {
                self.title = str_field(&event.body, "title");
                self.description = str_field(&event.body, "description");
                self.labels = labels_from(&event.body);
                self.source_ref = str_field(&event.body, "source_ref");
                self.base_ref = str_field(&event.body, "base_ref");
                self.source_head = str_field(&event.body, "source_head");
                self.base_head = str_field(&event.body, "base_head");
                self.merge_base = str_field(&event.body, "merge_base");
                self.closes = event.body.get("closes").and_then(JsonValue::as_u64);
                // t8 (#8): the creating command's role, when it stamped one.
                self.creator_role = str_field(&event.body, "role");
            }
            EventKind::PrComment => {
                if let Some(body) = str_field(&event.body, "body") {
                    self.comments.push(body);
                }
            }
            EventKind::PrReview => {
                if let Some(decision) = str_field(&event.body, "decision") {
                    self.effective_decision = Some(decision);
                    // Generation binding: a decision binds the snapshot
                    // generation current at review time; events without the
                    // field (pre-#1 chains) bind the generation current in
                    // the fold, which keeps legacy gen-0 chains valid.
                    self.decision_generation = generation_field(&event.body, self.generation);
                    // #2 result-bound review: the decision binds the live
                    // base-branch tip it was taken against; events without
                    // the field (pre-#2 chains) fold as None and only the
                    // unmoved-base merge path accepts them.
                    self.decision_base_head = str_field(&event.body, "base_head");
                    // Candidate-bound review: a decision taken while a
                    // prepared candidate exists records the candidate
                    // identity it approves. Legacy decisions fold as None
                    // and only the unmoved-base legacy path accepts them.
                    self.decision_strategy = str_field(&event.body, "strategy");
                    self.decision_result_commit = str_field(&event.body, "result_commit");
                    self.decision_result_tree = str_field(&event.body, "result_tree");
                    // t8 (#8): the reviewing command's role, when it
                    // stamped one — the self-approval gate reads this.
                    self.decision_role = str_field(&event.body, "role");
                }
            }
            EventKind::PrMerge => {
                self.merge_result = str_field(&event.body, "result_commit");
            }
            EventKind::PrAbandoned => {
                self.abandoned = str_field(&event.body, "reason");
            }
            EventKind::PrUpdate => {
                // Re-pin: the new snapshot generation replaces the old
                // source_head; base_head/merge_base are recomputed by the
                // writer (base movement is #2's live-base refresh, not
                // update's job). The generation-addressable refs pin every
                // generation immutably; the fold exposes only the latest.
                self.source_head = str_field(&event.body, "new_source_head");
                self.base_head = str_field(&event.body, "new_base_head");
                self.merge_base = str_field(&event.body, "new_merge_base");
                self.generation = generation_field(&event.body, self.generation + 1);
            }
            EventKind::PrPrepared => {
                // Last-wins: the latest `pr.prepared` event is the PR's
                // current candidate. Re-prepare replaces it; `pr.update`
                // bumps the generation so the gate sees the candidate as
                // stale (its prepared_generation no longer matches).
                self.prepared = Some(PreparedCandidate {
                    generation: generation_field(&event.body, self.generation),
                    source_head: str_field(&event.body, "source_head").unwrap_or_default(),
                    base_head: str_field(&event.body, "base_head").unwrap_or_default(),
                    strategy: str_field(&event.body, "strategy").unwrap_or_default(),
                    result_commit: str_field(&event.body, "result_commit").unwrap_or_default(),
                    result_tree: str_field(&event.body, "result_tree").unwrap_or_default(),
                    merge_base: str_field(&event.body, "merge_base").unwrap_or_default(),
                });
            }
            EventKind::CiCheck => {
                // The fold keeps the LATEST CI Check: each event overwrites the
                // prior status/plan/actor/ts, so the PR state always exposes
                // the most recent CI outcome (the merge-gate read).
                self.ci_status = str_field(&event.body, "status");
                self.ci_plan = str_field(&event.body, "plan");
                self.ci_actor = Some(event.actor.clone());
                self.ci_ts = Some(event.ts.clone());
                // Generation binding (same default rule as PrReview).
                self.ci_generation = generation_field(&event.body, self.generation);
                // #2 result-bound CI: the Check binds the live base-branch
                // tip it ran against (same None default as PrReview).
                self.ci_base_head = str_field(&event.body, "base_head");
                // Candidate-bound CI: a Check run against a prepared
                // candidate records the candidate identity it validated.
                // Legacy source-snapshot Checks fold these as None.
                self.ci_strategy = str_field(&event.body, "strategy");
                self.ci_result_commit = str_field(&event.body, "result_commit");
                self.ci_result_tree = str_field(&event.body, "result_tree");
            }
            _ => {}
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FoldState {
    pub issue: IssueState,
    pub pr: PrState,
}

pub fn fold(events: &[Event]) -> FoldState {
    let mut state = FoldState::default();
    for event in events {
        match event.entity.as_str() {
            "issue" => {
                state.issue.id = event.entity_id;
                match event.kind {
                    EventKind::IssueCreated => {
                        state.issue.title = event
                            .body
                            .get("title")
                            .and_then(JsonValue::as_str)
                            .map(String::from);
                        state.issue.description = event
                            .body
                            .get("description")
                            .and_then(JsonValue::as_str)
                            .map(String::from);
                        state.issue.labels = labels_from(&event.body);
                        state.issue.key = str_field(&event.body, "key");
                        state.issue.parent = event.body.get("parent").and_then(JsonValue::as_u64);
                        state.issue.blocked_by = issue_blockers_from(&event.body);
                        state.issue.open = true;
                    }
                    EventKind::IssueUpdated => {
                        if event.body.contains_key("title") {
                            state.issue.title = str_field(&event.body, "title");
                        }
                        if event.body.contains_key("description") {
                            state.issue.description = str_field(&event.body, "description");
                        }
                        if event.body.contains_key("labels") {
                            state.issue.labels = labels_from(&event.body);
                        }
                        if event.body.contains_key("parent") {
                            state.issue.parent =
                                event.body.get("parent").and_then(JsonValue::as_u64);
                        }
                        if event.body.contains_key("blocked_by") {
                            state.issue.blocked_by = issue_blockers_from(&event.body);
                        }
                    }
                    EventKind::IssueComment => {
                        if let Some(body) = event.body.get("body").and_then(JsonValue::as_str) {
                            state.issue.comments.push(body.to_string());
                        }
                    }
                    EventKind::IssueClose => state.issue.open = false,
                    EventKind::IssueReopen => state.issue.open = true,
                    _ => {}
                }
            }
            "pr" => {
                state.pr.id = event.entity_id;
                state.pr.apply(event);
            }
            _ => {}
        }
    }
    state
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SeqState {
    pub next: u64,
}

pub fn first_allocation() -> (u64, SeqState) {
    (1, SeqState { next: 2 })
}

/// Extract the `labels` array from an event body. Only string items are kept;
/// a missing/non-array/non-string `labels` field yields an empty vec (lenient
/// forward-compat: unknown or malformed payloads never break the fold).
fn labels_from(body: &HashMap<String, JsonValue>) -> Vec<String> {
    match body.get("labels") {
        Some(JsonValue::Array(items)) => items
            .iter()
            .filter_map(JsonValue::as_str)
            .map(String::from)
            .collect(),
        _ => Vec::new(),
    }
}

fn issue_blockers_from(body: &HashMap<String, JsonValue>) -> Vec<u64> {
    match body.get("blocked_by") {
        Some(JsonValue::Array(items)) => items.iter().filter_map(JsonValue::as_u64).collect(),
        _ => Vec::new(),
    }
}

/// Build a `JsonValue::Array` of string items — the single serialization
/// source for label arrays, shared by the CLI and store so `issue.created`
/// and `pr.created` wire payloads never drift.
pub fn json_string_array(items: &[String]) -> JsonValue {
    JsonValue::Array(items.iter().map(|s| JsonValue::String(s.clone())).collect())
}

/// The wire string serializer, re-exported for the machine interface (#7):
/// the `--json` output surface escapes through THIS function so the event
/// wire and the machine surface share one escape implementation.
pub(crate) fn json_escape_for_machine(value: &str) -> String {
    json_string(value)
}

fn json_string(value: &str) -> String {
    let mut out = String::from("\"");
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            '\u{0008}' => out.push_str("\\b"),
            '\u{000C}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => {
                // Unescaped C0 controls (other than the short forms above) are
                // invalid in JSON; emit the \u00XX form so stored events stay
                // standards-compliant and round-trip through any parser.
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn json_object(map: &HashMap<String, JsonValue>) -> String {
    let mut out = String::from("{");
    let mut first = true;
    for (k, v) in map {
        if !first {
            out.push(',');
        }
        first = false;
        out.push_str(&json_string(k));
        out.push(':');
        out.push_str(&json_value(v));
    }
    out.push('}');
    out
}

pub(crate) fn json_value(value: &JsonValue) -> String {
    match value {
        JsonValue::Null => "null".to_string(),
        JsonValue::Bool(b) => b.to_string(),
        JsonValue::Number(n) => n.to_string(),
        JsonValue::String(s) => json_string(s),
        JsonValue::Array(items) => {
            let mut out = String::from("[");
            let mut first = true;
            for item in items {
                if !first {
                    out.push(',');
                }
                first = false;
                out.push_str(&json_value(item));
            }
            out.push(']');
            out
        }
        JsonValue::Object(m) => json_object(m),
    }
}

struct Parser<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn new(input: &'a str) -> Self {
        Self {
            bytes: input.as_bytes(),
            pos: 0,
        }
    }

    fn skip_ws(&mut self) {
        while self.pos < self.bytes.len()
            && matches!(self.bytes[self.pos], b' ' | b'\t' | b'\r' | b'\n')
        {
            self.pos += 1;
        }
    }

    fn peek(&mut self) -> Option<u8> {
        self.skip_ws();
        self.bytes.get(self.pos).copied()
    }

    fn expect(&mut self, byte: u8) -> bool {
        if self.peek() == Some(byte) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn parse_value(&mut self) -> Option<JsonValue> {
        match self.peek()? {
            b'{' => self.parse_object(),
            b'[' => self.parse_array(),
            b'"' => Some(JsonValue::String(self.parse_string()?)),
            b't' => {
                if self.consume_literal("true") {
                    Some(JsonValue::Bool(true))
                } else {
                    None
                }
            }
            b'f' => {
                if self.consume_literal("false") {
                    Some(JsonValue::Bool(false))
                } else {
                    None
                }
            }
            b'n' => {
                if self.consume_literal("null") {
                    Some(JsonValue::Null)
                } else {
                    None
                }
            }
            b'-' | b'0'..=b'9' => self.parse_number(),
            _ => None,
        }
    }

    fn consume_literal(&mut self, lit: &str) -> bool {
        self.skip_ws();
        if self.bytes.get(self.pos..self.pos + lit.len()) == Some(lit.as_bytes()) {
            self.pos += lit.len();
            true
        } else {
            false
        }
    }

    fn parse_object(&mut self) -> Option<JsonValue> {
        self.pos += 1; // {
        let mut map = HashMap::new();
        self.skip_ws();
        if self.expect(b'}') {
            return Some(JsonValue::Object(map));
        }
        loop {
            self.skip_ws();
            let key = self.parse_string()?;
            if !self.expect(b':') {
                return None;
            }
            let value = self.parse_value()?;
            map.insert(key, value);
            self.skip_ws();
            if self.expect(b'}') {
                return Some(JsonValue::Object(map));
            }
            if !self.expect(b',') {
                return None;
            }
        }
    }

    fn parse_array(&mut self) -> Option<JsonValue> {
        self.pos += 1; // [
        let mut items = Vec::new();
        self.skip_ws();
        if self.expect(b']') {
            return Some(JsonValue::Array(items));
        }
        loop {
            let value = self.parse_value()?;
            items.push(value);
            self.skip_ws();
            if self.expect(b']') {
                return Some(JsonValue::Array(items));
            }
            if !self.expect(b',') {
                return None;
            }
        }
    }

    fn parse_string(&mut self) -> Option<String> {
        self.skip_ws();
        if self.peek()? != b'"' {
            return None;
        }
        self.pos += 1;
        // Accumulate raw bytes (including multi-byte UTF-8 from the input) and
        // decode once at the end. Decoding byte-by-byte via `c as char` would
        // turn every non-ASCII byte into a Latin-1 char (mojibake); the final
        // from_utf8 validates the sequence and yields the real string.
        let mut bytes: Vec<u8> = Vec::new();
        while self.pos < self.bytes.len() {
            let c = self.bytes[self.pos];
            if c == b'"' {
                self.pos += 1;
                let s = std::str::from_utf8(&bytes).ok()?;
                return Some(s.to_string());
            }
            if c == b'\\' {
                self.pos += 1;
                let esc = *self.bytes.get(self.pos)?;
                self.pos += 1;
                let decoded: char = match esc {
                    b'n' => '\n',
                    b't' => '\t',
                    b'r' => '\r',
                    b'b' => '\u{0008}',
                    b'f' => '\u{000C}',
                    b'\\' => '\\',
                    b'"' => '"',
                    b'/' => '/',
                    b'u' => {
                        // \uXXXX — four hex digits, decoded to a Unicode
                        // scalar. JSON \u escapes may encode a UTF-16 surrogate
                        // pair (supplementary-plane chars): a high surrogate
                        // must be immediately followed by \uXXXX low surrogate
                        // and the pair combines into one scalar. A lone
                        // low-surrogate (or a high surrogate not followed by a
                        // low surrogate) is invalid and rejected.
                        let code = self.read_hex4()?;
                        let code = if (0xD800..=0xDBFF).contains(&code) {
                            // The low surrogate must be IMMEDIATELY adjacent
                            // (`\uD83D\uDE00`): JSON allows no whitespace between
                            // the two escapes, so use direct byte access instead
                            // of `peek()` (which skips spaces/tabs/CR/LF).
                            if self.bytes.get(self.pos).copied() != Some(b'\\') {
                                return None;
                            }
                            self.pos += 1;
                            if self.bytes.get(self.pos).copied() != Some(b'u') {
                                return None;
                            }
                            self.pos += 1;
                            let low = self.read_hex4()?;
                            if !(0xDC00..=0xDFFF).contains(&low) {
                                return None;
                            }
                            0x10000 + ((code - 0xD800) << 10) + (low - 0xDC00)
                        } else if (0xDC00..=0xDFFF).contains(&code) {
                            return None; // lone low surrogate
                        } else {
                            code
                        };
                        char::from_u32(code)?
                    }
                    _ => return None,
                };
                let mut buf = [0u8; 4];
                bytes.extend_from_slice(decoded.encode_utf8(&mut buf).as_bytes());
                continue;
            }
            // A raw C0 control byte inside a string is invalid JSON — JSON
            // requires every U+0000–U+001F character to be escaped. The bytes
            // here are never produced by our serializer (json_string escapes
            // them), so rejecting them enforces wire-contract validity on
            // externally authored events. Escaped forms (`\n`, `\b`, `\u0001`)
            // are decoded above and are unaffected.
            if c < 0x20 {
                return None;
            }
            bytes.push(c);
            self.pos += 1;
        }
        None
    }

    /// Read exactly four hex digits as a `u32` (used by `\u` escapes). Advances
    /// `pos` past them; returns `None` on any malformed/truncated input.
    fn read_hex4(&mut self) -> Option<u32> {
        let end = self.pos + 4;
        if end > self.bytes.len() {
            return None;
        }
        let hex = std::str::from_utf8(&self.bytes[self.pos..end]).ok()?;
        let code = u32::from_str_radix(hex, 16).ok()?;
        self.pos = end;
        Some(code)
    }

    fn parse_number(&mut self) -> Option<JsonValue> {
        self.skip_ws();
        let start = self.pos;
        if self.peek() == Some(b'-') {
            self.pos += 1;
        }
        let digits_start = self.pos;
        while self.bytes.get(self.pos).is_some_and(|b| b.is_ascii_digit()) {
            self.pos += 1;
        }
        if self.pos == digits_start {
            return None;
        }
        let raw = std::str::from_utf8(&self.bytes[start..self.pos]).ok()?;
        raw.parse::<i64>().ok().map(JsonValue::Number)
    }
}

pub(crate) fn parse_json_value(input: &str) -> Option<JsonValue> {
    let mut parser = Parser::new(input);
    let value = parser.parse_value()?;
    parser.skip_ws();
    if parser.pos != parser.bytes.len() {
        return None;
    }
    Some(value)
}
#[cfg(test)]
#[path = "event_tests.rs"]
mod tests;
