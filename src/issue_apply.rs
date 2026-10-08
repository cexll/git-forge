//! Declarative issue graph reconciliation and the shared issue creation path.
use crate::event::{Event, EventKind, IssueState, JsonValue};
use crate::issue_plan::{self, Entry};
use crate::store::{EventStore, IssueSnapshot};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

struct Change {
    key: String,
    id: u64,
    action: &'static str,
    fields: Vec<&'static str>,
}
struct Plan {
    states: BTreeMap<u64, IssueState>,
    events: BTreeMap<u64, Vec<Event>>,
    changes: Vec<Change>,
    next: u64,
}
fn reference(
    value: &str,
    ids: &HashMap<String, u64>,
    snapshot: &IssueSnapshot,
) -> Result<u64, String> {
    if let Some(id) = value.strip_prefix('#') {
        let id = crate::cli::parse_entity_id(id)?;
        if !snapshot.states.contains_key(&id) {
            return Err(format!("issue #{id} does not exist"));
        }
        Ok(id)
    } else {
        ids.get(value)
            .copied()
            .ok_or_else(|| format!("unknown key '{value}'"))
    }
}
fn prepare(entries: &[Entry], snapshot: &IssueSnapshot, actor: &str) -> Result<Plan, String> {
    let mut existing_keys = HashMap::new();
    for state in snapshot.states.values() {
        if let Some(key) = &state.key {
            if existing_keys.insert(key.clone(), state.id).is_some() {
                return Err(format!("stored key '{key}' is ambiguous"));
            }
        }
    }
    let mut ids = HashMap::new();
    let mut next = snapshot.next;
    for entry in entries {
        let id = if let Some(id) = existing_keys.get(&entry.key) {
            *id
        } else {
            let id = next;
            next = next
                .checked_add(1)
                .filter(|n| *n <= i64::MAX as u64)
                .ok_or_else(|| entry.error("key", "issue counter exhausted"))?;
            if snapshot.tips.contains_key(&id) {
                return Err(entry.error("key", "counter collides with an existing issue ref"));
            }
            id
        };
        ids.insert(entry.key.clone(), id);
    }
    let mut plan = Plan {
        states: snapshot.states.clone(),
        events: BTreeMap::new(),
        changes: Vec::new(),
        next,
    };
    for entry in entries {
        let id = ids[&entry.key];
        let parent = entry
            .parent
            .as_ref()
            .map(|r| reference(r, &ids, snapshot).map_err(|e| entry.error("parent", &e)))
            .transpose()?;
        let mut blockers = entry
            .blocked_by
            .iter()
            .map(|r| reference(r, &ids, snapshot).map_err(|e| entry.error("blocked_by", &e)))
            .collect::<Result<Vec<_>, _>>()?;
        blockers.sort_unstable();
        if blockers.windows(2).any(|w| w[0] == w[1]) {
            return Err(entry.error("blocked_by", "duplicate resolved issue"));
        }
        let old = snapshot.states.get(&id);
        let state = IssueState {
            id,
            key: Some(entry.key.clone()),
            title: Some(entry.title.clone()),
            description: Some(entry.description.clone()),
            labels: entry.labels.clone(),
            parent,
            blocked_by: blockers,
            open: entry.open.unwrap_or_else(|| old.is_none_or(|s| s.open)),
            comments: old.map_or_else(Vec::new, |s| s.comments.clone()),
        };
        record(&mut plan, state, old, actor);
    }
    let plan_ids: Vec<_> = entries.iter().map(|entry| ids[&entry.key]).collect();
    validate_graph(&plan.states, &plan_ids).map_err(|(cycle, field, message)| {
        let implicated = |entry: &&Entry| cycle.contains(&ids[&entry.key]);
        let changed_edge = |entry: &&Entry| {
            let id = ids[&entry.key];
            let desired = &plan.states[&id];
            snapshot.states.get(&id).is_none_or(|old| {
                if field == "parent" {
                    old.parent != desired.parent
                } else {
                    old.blocked_by != desired.blocked_by
                }
            })
        };
        entries
            .iter()
            .filter(implicated)
            .find(changed_edge)
            .or_else(|| entries.iter().find(implicated))
            .unwrap_or(&entries[0])
            .error(field, &message)
    })?;
    Ok(plan)
}
fn json_id(id: Option<u64>) -> JsonValue {
    id.map_or(JsonValue::Null, |n| JsonValue::Number(n as i64))
}
fn json_ids(ids: &[u64]) -> JsonValue {
    JsonValue::Array(ids.iter().map(|n| JsonValue::Number(*n as i64)).collect())
}
fn record(plan: &mut Plan, state: IssueState, old: Option<&IssueState>, actor: &str) {
    let created = old.is_none();
    let mut body = HashMap::new();
    let mut fields = Vec::new();
    let values = [
        (
            "title",
            JsonValue::String(state.title.clone().unwrap_or_default()),
            old.is_none_or(|s| s.title != state.title),
        ),
        (
            "description",
            JsonValue::String(state.description.clone().unwrap_or_default()),
            old.is_none_or(|s| {
                s.description.as_deref().unwrap_or("") != state.description.as_deref().unwrap_or("")
            }),
        ),
        (
            "labels",
            crate::event::json_string_array(&state.labels),
            old.is_none_or(|s| s.labels != state.labels),
        ),
        (
            "parent",
            json_id(state.parent),
            old.is_none_or(|s| s.parent != state.parent),
        ),
        (
            "blocked_by",
            json_ids(&state.blocked_by),
            old.is_none_or(|s| s.blocked_by != state.blocked_by),
        ),
    ];
    for (name, value, changed) in values {
        if changed {
            fields.push(name);
            body.insert(name.to_string(), value);
        }
    }
    let mut events = Vec::new();
    if created {
        if let Some(key) = &state.key {
            body.insert("key".into(), JsonValue::String(key.clone()));
            fields.insert(0, "key");
        }
        events.push(Event::new(
            EventKind::IssueCreated,
            "issue",
            state.id,
            actor,
            body,
        ));
    } else if !body.is_empty() {
        events.push(Event::new(
            EventKind::IssueUpdated,
            "issue",
            state.id,
            actor,
            body,
        ));
    }
    if old.map_or(!state.open, |s| s.open != state.open) {
        fields.push("state");
        events.push(Event::new(
            if state.open {
                EventKind::IssueReopen
            } else {
                EventKind::IssueClose
            },
            "issue",
            state.id,
            actor,
            HashMap::new(),
        ));
    }
    plan.changes.push(Change {
        key: state.key.clone().unwrap_or_default(),
        id: state.id,
        action: if created {
            "create"
        } else if events.is_empty() {
            "unchanged"
        } else {
            "update"
        },
        fields,
    });
    if !events.is_empty() {
        plan.events.insert(state.id, events);
    }
    plan.states.insert(state.id, state);
}

/// Validate each graph separately, including edges through existing issues.
fn validate_graph(
    states: &BTreeMap<u64, IssueState>,
    roots: &[u64],
) -> Result<(), (Vec<u64>, &'static str, String)> {
    for field in ["parent", "blocked_by"] {
        let mut done = HashSet::new();
        for start in roots.iter().chain(states.keys()).copied() {
            if done.contains(&start) {
                continue;
            }
            let mut active = HashSet::new();
            let mut path = Vec::new();
            let mut stack = vec![(start, false)];
            while let Some((id, leaving)) = stack.pop() {
                if leaving {
                    active.remove(&id);
                    path.pop();
                    done.insert(id);
                    continue;
                }
                if active.contains(&id) {
                    let cycle = path
                        .iter()
                        .position(|node| *node == id)
                        .map(|index| path[index..].to_vec())
                        .unwrap_or_else(|| vec![id]);
                    return Err((cycle, field, format!("cycle involving issue #{id}")));
                }
                if done.contains(&id) {
                    continue;
                }
                let state = states
                    .get(&id)
                    .ok_or_else(|| (vec![start], field, format!("issue #{id} does not exist")))?;
                active.insert(id);
                path.push(id);
                stack.push((id, true));
                if field == "parent" {
                    if let Some(parent) = state.parent {
                        stack.push((parent, false));
                    }
                } else {
                    for blocker in state.blocked_by.iter().rev() {
                        stack.push((*blocker, false));
                    }
                }
            }
        }
    }
    Ok(())
}
fn render(plan: &Plan, json: bool) -> String {
    if json {
        let rows: Vec<_> = plan
            .changes
            .iter()
            .map(|c| {
                format!(
                    "{{\"key\":{},\"id\":{},\"action\":\"{}\",\"changed_fields\":[{}]}}",
                    crate::event::json_escape_for_machine(&c.key),
                    c.id,
                    c.action,
                    c.fields
                        .iter()
                        .map(|f| crate::event::json_escape_for_machine(f))
                        .collect::<Vec<_>>()
                        .join(",")
                )
            })
            .collect();
        format!("[{}]", rows.join(","))
    } else {
        plan.changes
            .iter()
            .map(|c| {
                format!(
                    "{} #{} {} [{}]",
                    c.action,
                    c.id,
                    crate::cli::sanitize_terminal(&c.key),
                    c.fields.join(", ")
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}
pub(crate) fn cmd_apply(args: &[String]) -> Result<String, String> {
    let (json, args) = crate::machine::json_requested(args);
    let mut dry = false;
    let mut path = None;
    for arg in &args {
        match arg.as_str() {
            "--dry-run" => dry = true,
            a if a.starts_with('-') => return Err(format!("unknown option '{a}'; usage: git forge issue apply <dir|file> [--dry-run] [--json]")),
            _ if path.is_none() => path = Some(arg),
            _ => return Err("issue apply: expected one plan path".into()),
        }
    }
    let entries = issue_plan::load(Path::new(path.ok_or("issue apply: expected a plan path")?))?;
    if dry {
        let store = EventStore::open(".").map_err(|e| e.to_string())?;
        let snapshot = store.issue_snapshot().map_err(|e| e.to_string())?;
        return Ok(render(&prepare(&entries, &snapshot, "dry-run")?, json));
    }
    let (store, actor) = crate::identity::open_mutation_store()?;
    let snapshot = store.store().issue_snapshot().map_err(|e| e.to_string())?;
    let plan = prepare(&entries, &snapshot, &actor)?;
    store
        .apply_issue_batch(&snapshot, plan.next, &plan.events)
        .map_err(|e| format!("issue apply: atomic transaction failed: {e}"))?;
    Ok(render(&plan, json))
}
pub(crate) fn new_issue(
    title: String,
    description: Option<String>,
    labels: Vec<String>,
    parent: Option<u64>,
    mut blocked_by: Vec<u64>,
    json: bool,
) -> Result<String, String> {
    let (store, actor) = crate::identity::open_mutation_store()?;
    blocked_by.sort_unstable();
    if blocked_by.windows(2).any(|w| w[0] == w[1]) {
        return Err("issue new: blocked_by: duplicate issue".into());
    }
    for _ in 0..crate::store::MAX_ALLOC_RETRIES {
        let snapshot = store.store().issue_snapshot().map_err(|e| e.to_string())?;
        let id = snapshot.next;
        if snapshot.tips.contains_key(&id) {
            return Err("issue new: counter collides with an existing issue ref".into());
        }
        let next = id
            .checked_add(1)
            .filter(|n| *n <= i64::MAX as u64)
            .ok_or("issue counter exhausted")?;
        let state = IssueState {
            id,
            title: Some(title.clone()),
            description: description.clone(),
            labels: labels.clone(),
            parent,
            blocked_by: blocked_by.clone(),
            open: true,
            ..IssueState::default()
        };
        let mut plan = Plan {
            states: snapshot.states.clone(),
            events: BTreeMap::new(),
            changes: Vec::new(),
            next,
        };
        record(&mut plan, state.clone(), None, &actor);
        validate_graph(&plan.states, &[id])
            .map_err(|(_, field, e)| format!("issue new: {field}: {e}"))?;
        match store.apply_issue_batch(&snapshot, next, &plan.events) {
            Ok(()) => {}
            Err(crate::store::StoreError::CasConflict) => continue,
            Err(error) => return Err(error.to_string()),
        }
        return if json {
            Ok(crate::machine::issue_json(
                &state,
                &plan.states.into_values().collect::<Vec<_>>(),
            ))
        } else {
            Ok(format!("issue #{id} created: {title}"))
        };
    }
    Err(crate::store::StoreError::Exhausted.to_string())
}
