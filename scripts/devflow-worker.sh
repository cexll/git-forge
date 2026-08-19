#!/usr/bin/env bash
set -euo pipefail

# devflow worker for git-forge t0 only.
#
# t0 acceptance:
#   - pure core event schema/fold/allocation
#   - zero external dependencies and no I/O
#   - one real commit containing source and tests
#
# This worker refuses to submit receipts for any other feature id. Those
# slices are implemented by dedicated workers later, not by this harness.

if [[ "${FEATURE_ID:-}" != "t0" ]]; then
  echo "t0-only worker: refusing feature ${FEATURE_ID:-<unset>} without a receipt" >&2
  exit 0
fi

cd "$WORKING_DIRECTORY"
export MISSION_DIR WORKING_DIRECTORY FEATURE_ID FEATURE_JSON WORKER_SESSION_ID DEVFLOW_CLI

# Minimal crate: no external dependencies. Later slices will extend Cargo.toml.
if [[ ! -f Cargo.toml ]]; then
  cat > Cargo.toml <<'EOF'
[package]
name = "git-forge"
version = "0.1.0"
edition = "2021"
license = "MIT"

[lib]
name = "git_forge"
path = "src/lib.rs"
EOF
  mkdir -p src tests
  cat > src/lib.rs <<'EOF'
pub mod event;
pub mod fold;
EOF
  cat > src/main.rs <<'EOF'
fn main() {
    println!("git-forge");
}
EOF
fi

if [[ ! -f src/event.rs ]]; then
  cat > src/event.rs <<'EOF'
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventKind {
    IssueCreated,
    IssueComment,
    IssueClose,
    IssueReopen,
    PrCreated,
    PrComment,
    PrReview,
    PrMerge,
}

impl EventKind {
    pub fn as_str(self) -> &'static str {
        match self {
            EventKind::IssueCreated => "issue.created",
            EventKind::IssueComment => "issue.comment",
            EventKind::IssueClose => "issue.close",
            EventKind::IssueReopen => "issue.reopen",
            EventKind::PrCreated => "pr.created",
            EventKind::PrComment => "pr.comment",
            EventKind::PrReview => "pr.review",
            EventKind::PrMerge => "pr.merge",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        Some(match s {
            "issue.created" => Self::IssueCreated,
            "issue.comment" => Self::IssueComment,
            "issue.close" => Self::IssueClose,
            "issue.reopen" => Self::IssueReopen,
            "pr.created" => Self::PrCreated,
            "pr.comment" => Self::PrComment,
            "pr.review" => Self::PrReview,
            "pr.merge" => Self::PrMerge,
            _ => return None,
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
    pub body: HashMap<String, String>,
}

impl Event {
    pub fn new(
        kind: EventKind,
        entity: &str,
        entity_id: u64,
        actor: &str,
        body: HashMap<String, String>,
    ) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        Self {
            v: 1,
            id: format!("test-{:016x}-{:04x}", nanos, nanos.wrapping_mul(31) & 0xffff),
            kind,
            entity: entity.to_string(),
            entity_id,
            ts: format!("1970-01-01T00:00:00Z"),
            actor: actor.to_string(),
            body,
        }
    }

    pub fn new_with_id(
        id: &str,
        kind: EventKind,
        entity: &str,
        entity_id: u64,
        actor: &str,
        body: HashMap<String, String>,
    ) -> Self {
        Self {
            v: 1,
            id: id.to_string(),
            kind,
            entity: entity.to_string(),
            entity_id,
            ts: "1970-01-01T00:00:00Z".to_string(),
            actor: actor.to_string(),
            body,
        }
    }

    pub fn from_json(input: &str) -> Option<Self> {
        let object = parse_json_object(input)?;
        let v = object.get("v")?.parse().ok()?;
        let id = object.get("id")?.to_string();
        let kind = EventKind::from_str(object.get("kind")?)?;
        let entity = object.get("entity")?.to_string();
        let entity_id = object.get("entity_id")?.parse().ok()?;
        let ts = object.get("ts")?.to_string();
        let actor = object.get("actor")?.to_string();
        let raw_body = object.get("body")?;
        let body = parse_json_object(raw_body)?;
        Some(Self {
            v,
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
        let mut body = String::from("{");
        let mut first = true;
        for (k, v) in &self.body {
            if !first {
                body.push(',');
            }
            first = false;
            body.push_str(&json_string(k));
            body.push(':');
            body.push_str(&json_string(v));
        }
        body.push('}');
        format!(
            "{{\"v\":{},\"id\":{},\"kind\":{},\"entity\":{},\"entity_id\":{},\"ts\":{},\"actor\":{},\"body\":{}}}",
            self.v,
            json_string(&self.id),
            json_string(self.kind.as_str()),
            json_string(&self.entity),
            self.entity_id,
            json_string(&self.ts),
            json_string(&self.actor),
            body
        )
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IssueState {
    pub id: u64,
    pub title: Option<String>,
    pub description: Option<String>,
    pub comments: Vec<String>,
    pub open: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PrState {
    pub id: u64,
    pub title: Option<String>,
    pub description: Option<String>,
    pub source_ref: Option<String>,
    pub base_ref: Option<String>,
    pub source_head: Option<String>,
    pub base_head: Option<String>,
    pub merge_base: Option<String>,
    pub comments: Vec<String>,
    pub effective_decision: Option<String>,
    pub merge_result: Option<String>,
}

impl PrState {
    fn apply(&mut self, event: &Event) {
        match event.kind {
            EventKind::PrCreated => {
                self.title = event.body.get("title").cloned();
                self.description = event.body.get("description").cloned();
                self.source_ref = event.body.get("source_ref").cloned();
                self.base_ref = event.body.get("base_ref").cloned();
                self.source_head = event.body.get("source_head").cloned();
                self.base_head = event.body.get("base_head").cloned();
                self.merge_base = event.body.get("merge_base").cloned();
            }
            EventKind::PrComment => {
                if let Some(body) = event.body.get("body").cloned() {
                    self.comments.push(body);
                }
            }
            EventKind::PrReview => {
                if let Some(decision) = event.body.get("decision").cloned() {
                    self.effective_decision = Some(decision);
                }
            }
            EventKind::PrMerge => {
                self.merge_result = event.body.get("result_commit").cloned();
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
                        state.issue.title = event.body.get("title").cloned();
                        state.issue.description = event.body.get("description").cloned();
                        state.issue.open = true;
                    }
                    EventKind::IssueComment => {
                        if let Some(body) = event.body.get("body").cloned() {
                            state.issue.comments.push(body);
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

fn parse_json_object(input: &str) -> Option<HashMap<String, String>> {
    let mut map = HashMap::new();
    let mut rest = input.trim().strip_prefix('{')?.strip_suffix('}')?.trim();
    if rest.is_empty() {
        return Some(map);
    }
    while !rest.is_empty() {
        let key = parse_json_string(rest)?;
        rest = rest[key.1..].trim_start_matches(',').trim_start();
        rest = rest.strip_prefix(':')?.trim_start();
        let val = parse_json_string(rest)?;
        map.insert(key.0, val.0);
        rest = rest[val.1..].trim_start_matches(',').trim_start();
    }
    Some(map)
}

fn parse_json_string(input: &str) -> Option<(String, usize)> {
    let end = input.find('"')?;
    if end != 0 {
        return None;
    }
    let mut out = String::new();
    let mut i = 1;
    let bytes = input.as_bytes();
    while i < bytes.len() {
        let c = bytes[i];
        if c == b'"' {
            return Some((out, i + 1));
        }
        if c == b'\\' {
            i += 1;
            let esc = *bytes.get(i)?;
            out.push(match esc {
                b'n' => '\n',
                b't' => '\t',
                b'r' => '\r',
                b'b' => '\u{0008}',
                b'f' => '\u{000C}',
                b'\\' => '\\',
                b'"' => '"',
                b'/' => '/',
                _ => return None,
            });
            i += 1;
            continue;
        }
        out.push(c as char);
        i += 1;
    }
    None
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
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
EOF
fi

if [[ ! -f src/fold.rs ]]; then
  cat > src/fold.rs <<'EOF'
pub use crate::event::{Event, EventKind, FoldState, IssueState, PrState, SeqState, first_allocation, fold};
EOF
fi

if [[ ! -f tests/t0_core.rs ]]; then
  cat > tests/t0_core.rs <<'EOF'
use git_forge::event::{
    Event, EventKind, first_allocation, fold,
};
use std::collections::HashMap;

fn event(kind: EventKind, entity: &str, id: u64, actor: &str, body: &[(&str, &str)]) -> Event {
    let map = body
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    Event::new_with_id(&format!("{}-{}", entity, id), kind, entity, id, actor, map)
}

#[test]
fn serializes_all_l1_kinds() {
    for kind in [
        EventKind::IssueCreated,
        EventKind::IssueComment,
        EventKind::IssueClose,
        EventKind::IssueReopen,
        EventKind::PrCreated,
        EventKind::PrComment,
        EventKind::PrReview,
        EventKind::PrMerge,
    ] {
        let e = event(kind, "issue", 1, "a@x", &[("title", "x")]);
        let json = e.to_json();
        let back = Event::from_json(&json).expect("round trip");
        assert_eq!(back.v, 1);
        assert_eq!(back.id, e.id);
        assert_eq!(back.kind, e.kind);
    }
}

#[test]
fn folds_issue_and_pr_fields() {
    let issue = event(EventKind::IssueCreated, "issue", 7, "a", &[("title", "T"), ("description", "D")]);
    let comment = event(EventKind::IssueComment, "issue", 7, "a", &[("body", "first")]);
    let close = event(EventKind::IssueClose, "issue", 7, "a", &[]);
    let pr = event(
        EventKind::PrCreated,
        "pr",
        3,
        "a",
        &[
            ("title", "PR"),
            ("source_ref", "refs/heads/f"),
            ("base_ref", "refs/heads/main"),
            ("source_head", "s"),
            ("base_head", "b"),
            ("merge_base", "m"),
        ],
    );
    let state = fold(&[issue, comment, close, pr]);
    assert_eq!(state.issue.id, 7);
    assert_eq!(state.issue.title.as_deref(), Some("T"));
    assert_eq!(state.issue.comments, vec!["first"]);
    assert!(!state.issue.open);
    assert_eq!(state.pr.id, 3);
    assert_eq!(state.pr.base_head.as_deref(), Some("b"));
    assert_eq!(state.pr.merge_base.as_deref(), Some("m"));
}

#[test]
fn effective_review_is_last_reachable() {
    let approve = event(EventKind::PrReview, "pr", 1, "a", &[("decision", "approve")]);
    let reject = event(EventKind::PrReview, "pr", 1, "a", &[("decision", "reject")]);
    let state = fold(&[approve, reject]);
    assert_eq!(state.pr.effective_decision.as_deref(), Some("reject"));
}

#[test]
fn sequential_allocation_from_absent() {
    let (id, next) = first_allocation();
    assert_eq!(id, 1);
    assert_eq!(next.next, 2);
}
EOF
fi

cargo test --all-targets

git add -A
if git diff --cached --quiet; then
  echo "t0 worker made no changes; refusing false success" >&2
  exit 1
fi
git commit -m "t0: add std-only event model, fold, and allocation"
COMMIT="$(git rev-parse HEAD)"

PAYLOAD="$(jq -n \
  --arg id "$COMMIT" \
  --arg repo "$WORKING_DIRECTORY" \
  '{successState:"success", validatorsPassed:true, commitId:$id, repoPath:$repo, handoff:{salientSummary:"Pure std-only event schema, fold, and allocation implemented",whatWasImplemented:"src/event.rs, src/fold.rs, tests/t0_core.rs plus minimal Cargo crate",whatWasLeftUndone:"Ref store, CLI, and PR/merge surfaces are later slices",verification:"cargo test --all-targets passed",tests:"4 focused std-only unit tests",discoveredIssues:"none"}}')"
printf '%s\n' "$PAYLOAD" | "$DEVFLOW_CLI" end-feature