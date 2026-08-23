use git_forge::event::{first_allocation, fold, is_uuid_v4, Event, EventKind, JsonValue};
use std::collections::HashMap;

fn event(kind: EventKind, entity: &str, id: u64, actor: &str, body: &[(&str, JsonValue)]) -> Event {
    let map: HashMap<String, JsonValue> = body
        .iter()
        .map(|(k, v)| (k.to_string(), v.clone()))
        .collect();
    let numeric = entity
        .bytes()
        .fold(0u64, |acc, b| acc.wrapping_mul(31).wrapping_add(b as u64));
    Event::new_with_id(
        &format!("11111111-1111-4111-8111-{:012x}", (numeric << 8) ^ id),
        kind,
        entity,
        id,
        actor,
        map,
    )
    .expect("fixture id is uuid v4")
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
        let e = event(
            kind,
            "issue",
            1,
            "a@x",
            &[("title", JsonValue::String("x".into()))],
        );
        let json = e.to_json();
        let back = Event::from_json(&json).expect("round trip");
        assert_eq!(back.v, 1);
        assert_eq!(back.id, e.id);
        assert_eq!(back.kind, e.kind);
        assert_eq!(back.body.get("title"), e.body.get("title"));
    }
}

#[test]
fn generated_id_is_uuid_v4_shape() {
    let e = Event::new(EventKind::IssueCreated, "issue", 1, "a@x", HashMap::new());
    assert!(is_uuid_v4(&e.id), "id={}", e.id);
    assert!(!is_uuid_v4("not-a-uuid"));
}

#[test]
fn rejects_non_v1_schema_and_bad_uuid() {
    let good = event(
        EventKind::IssueCreated,
        "issue",
        1,
        "a@x",
        &[("title", JsonValue::String("T".into()))],
    );
    let bad_version = good.to_json().replacen("\"v\":1", "\"v\":2", 1);
    assert!(Event::from_json(&bad_version).is_none());

    // Variant nibble '1' (4th group) is outside RFC 4122 variant [89ab];
    // note "badbadbadbad" would NOT be a valid negative fixture: b/a/d are all
    // hex digits, so it is a well-formed v4 shape and must be accepted.
    let bad_id = good
        .to_json()
        .replacen(&good.id, "11111111-1111-4111-1111-111111111111", 1);
    assert!(Event::from_json(&bad_id).is_none());

    assert!(Event::new_with_id(
        "not-a-uuid",
        EventKind::IssueCreated,
        "issue",
        1,
        "a@x",
        HashMap::new(),
    )
    .is_none());
}

#[test]
fn folds_issue_and_pr_fields() {
    let issue = event(
        EventKind::IssueCreated,
        "issue",
        7,
        "a",
        &[
            ("title", JsonValue::String("T".into())),
            ("description", JsonValue::String("D".into())),
        ],
    );
    let comment = event(
        EventKind::IssueComment,
        "issue",
        7,
        "a",
        &[("body", JsonValue::String("first".into()))],
    );
    let close = event(EventKind::IssueClose, "issue", 7, "a", &[]);
    let pr = event(
        EventKind::PrCreated,
        "pr",
        3,
        "a",
        &[
            ("title", JsonValue::String("PR".into())),
            ("source_ref", JsonValue::String("refs/heads/f".into())),
            ("base_ref", JsonValue::String("refs/heads/main".into())),
            ("source_head", JsonValue::String("s".into())),
            ("base_head", JsonValue::String("b".into())),
            ("merge_base", JsonValue::String("m".into())),
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
    let approve = event(
        EventKind::PrReview,
        "pr",
        1,
        "a",
        &[("decision", JsonValue::String("approve".into()))],
    );
    let reject = event(
        EventKind::PrReview,
        "pr",
        1,
        "a",
        &[("decision", JsonValue::String("reject".into()))],
    );
    let state = fold(&[approve, reject]);
    assert_eq!(state.pr.effective_decision.as_deref(), Some("reject"));
}

#[test]
fn sequential_allocation_from_absent() {
    let (id, next) = first_allocation();
    assert_eq!(id, 1);
    assert_eq!(next.next, 2);
}

/// JsonValue accessor fallbacks: as_str/as_u64/as_object return None for the
/// wrong variant, and as_u64 also rejects negative numbers.
#[test]
fn json_value_accessor_fallbacks_return_none() {
    assert!(JsonValue::Number(1).as_str().is_none());
    assert!(JsonValue::Null.as_str().is_none());
    assert!(JsonValue::Bool(true).as_str().is_none());
    assert!(JsonValue::Array(vec![]).as_str().is_none());
    assert!(JsonValue::String("x".into()).as_u64().is_none());
    assert!(JsonValue::Bool(true).as_u64().is_none());
    assert!(JsonValue::Number(-1).as_u64().is_none(), "negative not u64");
    assert!(JsonValue::Array(vec![]).as_object().is_none());
    assert!(JsonValue::Number(1).as_object().is_none());
    assert!(JsonValue::Null.as_object().is_none());
    assert_eq!(JsonValue::Number(42).as_u64(), Some(42));
}

/// from_json rejects a valid-JSON non-object body, an unknown wire kind, and
/// a missing/non-string id (the `?`/match arms, not parse errors).
#[test]
fn from_json_rejects_non_object_body_and_bad_fields() {
    let good = event(EventKind::IssueCreated, "issue", 1, "a@x", &[]).to_json();
    // well-formed body that is an array, not an object
    let non_obj = good.replacen("\"body\":{}", "\"body\":[]", 1);
    assert!(Event::from_json(&non_obj).is_none());
    // unknown wire kind
    let bad_kind = good.replacen("\"kind\":\"issue.created\"", "\"kind\":\"issue.bogus\"", 1);
    assert!(Event::from_json(&bad_kind).is_none());
    // id key absent
    let no_id = good.replacen("\"id\":", "\"no_id\":", 1);
    assert!(Event::from_json(&no_id).is_none());
    // id present but not a string
    let num_id = good.replacen("\"id\":\"", "\"id\":7,\"_x\":\"", 1);
    assert!(Event::from_json(&num_id).is_none());
}

/// fold ignores unknown entities and mismatched entity/kind pairs via the `_`
/// arms (issue entity + PR kind, PR entity + issue kind, unknown entity).
#[test]
fn fold_ignores_unknown_entities_and_mismatched_kinds() {
    let state = fold(&[
        event(EventKind::PrCreated, "issue", 5, "a@x", &[]),
        event(EventKind::IssueCreated, "pr", 9, "a@x", &[]),
        event(EventKind::IssueCreated, "widget", 2, "a@x", &[]),
    ]);
    // The issue entity id is set before the kind match, but no fields are
    // populated by a PR-kind event on an issue entity.
    assert_eq!(state.issue.id, 5);
    assert_eq!(state.issue.title, None);
    assert!(!state.issue.open);
    assert!(state.issue.comments.is_empty());
    // The pr entity id is set, but an issue-kind event fills no PR fields.
    assert_eq!(state.pr.id, 9);
    assert_eq!(state.pr.title, None);
    assert_eq!(state.pr.effective_decision, None);
}
