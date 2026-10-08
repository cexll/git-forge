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
        EventKind::IssueUpdated,
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
fn legacy_issue_logs_default_graph_fields() {
    let wire = r#"{"v":1,"id":"11111111-1111-4111-8111-111111111111","kind":"issue.created","entity":"issue","entity_id":7,"ts":"2026-10-08T00:00:00Z","actor":"a@x","body":{"title":"Legacy","description":"Body"}}"#;
    let created = Event::from_json(wire).expect("legacy schema v1 remains readable");
    let issue = fold(&[created]).issue;
    assert_eq!(issue.title.as_deref(), Some("Legacy"));
    assert_eq!(issue.description.as_deref(), Some("Body"));
    assert_eq!(issue.key, None);
    assert_eq!(issue.parent, None);
    assert!(issue.blocked_by.is_empty());
    assert!(issue.open);
}

#[test]
fn issue_updates_preserve_omitted_fields_and_clear_explicit_fields() {
    let created = event(
        EventKind::IssueCreated,
        "issue",
        7,
        "a@x",
        &[
            ("title", JsonValue::String("Ticket".into())),
            ("description", JsonValue::String("Body".into())),
            ("key", JsonValue::String("ticket".into())),
            ("parent", JsonValue::Number(1)),
            ("blocked_by", JsonValue::Array(vec![JsonValue::Number(2)])),
            (
                "labels",
                JsonValue::Array(vec![JsonValue::String("ready".into())]),
            ),
        ],
    );
    let close = event(EventKind::IssueClose, "issue", 7, "a@x", &[]);
    let update_wire = r#"{"v":1,"id":"22222222-2222-4222-8222-222222222222","kind":"issue.updated","entity":"issue","entity_id":7,"ts":"2026-10-08T00:00:01Z","actor":"a@x","body":{"title":"Changed"}}"#;
    let updated = Event::from_json(update_wire).expect("issue.updated parses");
    let issue = fold(&[created.clone(), close.clone(), updated.clone()]).issue;
    assert_eq!(issue.title.as_deref(), Some("Changed"));
    assert_eq!(issue.description.as_deref(), Some("Body"));
    assert_eq!(issue.key.as_deref(), Some("ticket"));
    assert_eq!(issue.parent, Some(1));
    assert_eq!(issue.blocked_by, vec![2]);
    assert_eq!(issue.labels, vec!["ready"]);
    assert!(!issue.open);
    let clear = event(
        updated.kind,
        "issue",
        7,
        "a@x",
        &[
            ("description", JsonValue::Null),
            ("parent", JsonValue::Null),
            ("labels", JsonValue::Array(vec![])),
            ("blocked_by", JsonValue::Array(vec![])),
            ("key", JsonValue::String("must-not-change".into())),
        ],
    );
    let issue = fold(&[created, close, updated, clear]).issue;
    assert_eq!(issue.title.as_deref(), Some("Changed"));
    assert_eq!(issue.key.as_deref(), Some("ticket"));
    assert_eq!(issue.description, None);
    assert_eq!(issue.parent, None);
    assert!(issue.labels.is_empty());
    assert!(issue.blocked_by.is_empty());
    assert!(!issue.open);
}

#[test]
fn issue_updates_replace_present_graph_fields() {
    let created = event(
        EventKind::IssueCreated,
        "issue",
        7,
        "a@x",
        &[("title", JsonValue::String("Ticket".into()))],
    );
    let updated = event(
        "issue.updated".parse().expect("update wire kind"),
        "issue",
        7,
        "a@x",
        &[
            ("description", JsonValue::String("New body".into())),
            (
                "labels",
                JsonValue::Array(vec![JsonValue::String("ready".into())]),
            ),
            ("parent", JsonValue::Number(1)),
            (
                "blocked_by",
                JsonValue::Array(vec![JsonValue::Number(2), JsonValue::Number(3)]),
            ),
        ],
    );
    let issue = fold(&[created, updated]).issue;
    assert_eq!(issue.title.as_deref(), Some("Ticket"));
    assert_eq!(issue.description.as_deref(), Some("New body"));
    assert_eq!(issue.labels, vec!["ready"]);
    assert_eq!(issue.parent, Some(1));
    assert_eq!(issue.blocked_by, vec![2, 3]);
    assert!(issue.open);
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

/// Non-ASCII (multi-byte UTF-8) strings must round-trip byte-identical.
/// Previously parse_string read each byte as a Latin-1 char (`c as char`),
/// so `café` serialized as valid UTF-8 came back as `cafÃ©`.
#[test]
fn utf8_strings_roundtrip_without_mojibake() {
    let e = event(
        EventKind::PrCreated,
        "pr",
        1,
        "dev@example.com",
        &[
            ("title", JsonValue::String("café 中文 🚀".into())),
            ("description", JsonValue::String("naïve — 東京".into())),
            (
                "labels",
                JsonValue::Array(vec![
                    JsonValue::String("标签 émoji 🚀".into()),
                    JsonValue::String("bug".into()),
                ]),
            ),
        ],
    );
    let json = e.to_json();
    // The wire form must carry the UTF-8 bytes unescaped-and-writable.
    let back = Event::from_json(&json).expect("roundtrip must parse non-ASCII");
    assert_eq!(back.body, e.body);
}

/// Raw C0 control characters (other than the JSON short forms) must be
/// written back-escaped (`\b`, `\f`, `\u00XX`), never as bare bytes — the
/// stored event must remain standards-compliant JSON.
#[test]
fn control_chars_are_escaped_in_wire_json() {
    let e = event(
        EventKind::IssueCreated,
        "issue",
        1,
        "a@x",
        &[(
            "title",
            JsonValue::String("a\u{0008}b\u{0001}c\u{000C}d".into()),
        )],
    );
    let json = e.to_json();
    // No unescaped control byte may appear (structural JSON whitespace is the
    // only legit <0x20 and is produced by the serializer's own formatting).
    let bare_control = json
        .bytes()
        .any(|b| b < 0x20 && !matches!(b, b'\n' | b't' | b'\t' | b'\r'));
    assert!(
        !bare_control,
        "wire JSON contains a bare control byte: {json:?}"
    );
    let back = Event::from_json(&json).expect("control chars must round-trip");
    assert_eq!(back.body, e.body);
}

#[test]
fn issue_new_graph_options_reject_missing_invalid_and_duplicate_parent() {
    for args in [
        vec!["new", "Ticket", "--parent"],
        vec!["new", "Ticket", "--blocked-by"],
        vec!["new", "Ticket", "--parent", "not-an-id"],
        vec!["new", "Ticket", "--blocked-by", "not-an-id"],
        vec!["new", "Ticket", "--parent", "1", "--parent", "2"],
    ] {
        let args: Vec<_> = args.into_iter().map(String::from).collect();
        assert!(
            git_forge::issue::run_issue(&args).is_err(),
            "args: {args:?}"
        );
    }
}

#[test]
fn issue_graph_help_describes_apply_and_filter_options() {
    for (subcommand, required) in [
        ("apply", vec!["<dir|file>", "--dry-run", "--json"]),
        ("new", vec!["--parent", "--blocked-by"]),
        ("list", vec!["--parent", "--state open|closed|all"]),
    ] {
        let help = git_forge::issue::run_issue(&[subcommand.into(), "--help".into()])
            .expect("help needs no repository");
        for flag in required {
            assert!(help.contains(flag), "missing {flag} in {help}");
        }
    }
}
