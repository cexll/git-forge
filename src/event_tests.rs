use super::*;
use std::collections::HashMap;

fn body_with(kv: Vec<(&str, JsonValue)>) -> HashMap<String, JsonValue> {
    kv.into_iter().map(|(k, v)| (k.to_string(), v)).collect()
}

/// v1 schema serialization is self-consistent: to_json -> from_json is the
/// identity on every field, including control characters that must be
/// escaped in the wire encoding (wire contract § Event JSON schema v1).
#[test]
fn json_roundtrip_escapes_control_chars_in_actor_and_body() {
    let ev = Event::new(
        EventKind::IssueComment,
        "issue",
        7,
        "dev@example.com",
        body_with(vec![
            ("title", JsonValue::String("a\"b\\c\nd\te\rf".into())),
            ("n", JsonValue::Number(-42)),
            ("ok", JsonValue::Bool(true)),
            ("none", JsonValue::Null),
        ]),
    );
    let json = ev.to_json();
    assert!(json.contains("\\\"") && json.contains("\\\\") && json.contains("\\n"));
    let round = Event::from_json(&json).expect("roundtrip must parse");
    assert_eq!(round, ev);
}

/// Nested arrays/objects survive serialization and parsing verbatim.
#[test]
fn json_roundtrip_nested_structures() {
    let nested = JsonValue::Array(vec![
        JsonValue::Object(body_with(vec![(
            "deep",
            JsonValue::Array(vec![JsonValue::Number(1), JsonValue::String("x".into())]),
        )])),
        JsonValue::Number(0),
    ]);
    let ev = Event::new(
        EventKind::PrReview,
        "pr",
        3,
        "a@b.c",
        body_with(vec![("review", nested)]),
    );
    let round = Event::from_json(&ev.to_json()).expect("nested roundtrip must parse");
    assert_eq!(round, ev);
}

/// from_json is strict: malformed shape, wrong schema version, non-UUID id,
/// unknown kind, or a non-object body must all be rejected, never coerced.
#[test]
fn from_json_rejects_malformed_shapes() {
    let good = Event::new(EventKind::IssueCreated, "issue", 1, "x@y.z", HashMap::new()).to_json();
    // Truncate the JSON to a malformed but parseable-then-absent tail.
    assert!(Event::from_json("not json").is_none());
    // strip the closing brace -> trailing garbage, parse must reject.
    assert!(Event::from_json(&good[..good.len() - 1]).is_none());
    // wrong schema version
    let wrong_v = good.replace("\"v\":1", "\"v\":2");
    assert!(Event::from_json(&wrong_v).is_none());
    // non-object body
    let bad_body = good.replace("\"body\":{", "\"body\":[");
    assert!(Event::from_json(&bad_body).is_none());
}

/// EventKind as_str/from_str roundtrip for every wire kind; unknown rejects.
#[test]
fn event_kind_string_roundtrip_all_kinds() {
    for kind in [
        EventKind::IssueCreated,
        EventKind::IssueComment,
        EventKind::IssueClose,
        EventKind::IssueReopen,
        EventKind::PrCreated,
        EventKind::PrComment,
        EventKind::PrReview,
        EventKind::PrMerge,
        EventKind::CiCheck,
    ] {
        assert_eq!(kind.as_str().parse::<EventKind>().unwrap(), kind);
    }
    assert!("ci.check".parse::<EventKind>().is_ok());
    assert!("pr.merge".parse::<EventKind>().is_ok());
    assert!("issue.bogus".parse::<EventKind>().is_err());
    assert!("".parse::<EventKind>().is_err());
}

/// UUID-v4 shape validation: exact length/hyphen positions, version nibble,
/// variant nibble, and hex-only rejection.
#[test]
fn uuid_v4_shape_boundaries() {
    assert!(is_uuid_v4("550e8400-e29b-41d4-a716-446655440000"));
    assert!(is_uuid_v4("00000000-0000-4000-8000-000000000000"));
    // wrong length
    assert!(!is_uuid_v4("550e8400-e29b-41d4-a716-44665544000"));
    // wrong hyphen position
    assert!(!is_uuid_v4("550e8400-e29b-41d4-a716446655440000"));
    // wrong version nibble (not 4)
    assert!(!is_uuid_v4("550e8400-e29b-51d4-a716-446655440000"));
    // wrong variant nibble (not 8/9/a/b)
    assert!(!is_uuid_v4("550e8400-e29b-41d4-1716-446655440000"));
    // non-hex char
    assert!(!is_uuid_v4("550e8400-e29b-41d4-a716-44665544000g"));
}

/// Full escape set (\b \f \/), negative numbers, empty containers, and
/// whitespace-tolerant parsing — branches not hit by the roundtrip tests.
#[test]
fn json_parser_accepts_full_escape_set_and_negative_numbers() {
    let v = parse_json_value(r#""\b\f\/""#).unwrap();
    assert_eq!(v.as_str().unwrap(), "\u{0008}\u{000C}/");
    assert_eq!(parse_json_value("-17").unwrap(), JsonValue::Number(-17));
    assert_eq!(parse_json_value("42").unwrap(), JsonValue::Number(42));
    // nested empty containers parse
    assert!(parse_json_value("{}").is_some());
    assert!(parse_json_value("[]").is_some());
    // whitespace around structure is tolerated
    assert!(parse_json_value(r#"  { "a" : [ 1 , 2 ] }  "#).is_some());
    // nested object/array round value
    let v = parse_json_value(r#"{"a":{"b":[true,null]}}"#).unwrap();
    assert!(v.as_object().is_some());
}

/// JsonValue accessor fallbacks are covered in tests/t0_core.rs.
#[test]
fn uuid_v4_rejects_wrong_char_at_hyphen_slot() {
    assert!(!is_uuid_v4("550e8400xe29b-41d4-a716-446655440000"));
    assert!(!is_uuid_v4("550e8400-e29b041d4-a716-446655440000"));
}

/// Parser edges: empty input (EOF peek), a `false` literal, a non-quoted
/// object key, and an integer that overflows i64 must all be handled.
#[test]
fn json_parser_empty_and_false_and_overflows() {
    assert!(parse_json_value("").is_none());
    assert!(parse_json_value("  \t\n ").is_none());
    assert_eq!(parse_json_value("false"), Some(JsonValue::Bool(false)));
    assert!(parse_json_value(r#"{"k": false}"#).is_some());
    // non-string object key
    assert!(parse_json_value("{123:1}").is_none());
    // overflow of i64 (well-formed digits, not parseable as i64)
    assert!(parse_json_value("9223372036854775808").is_none());
    assert!(parse_json_value("-9223372036854775809").is_none());
}

/// Strict rejection: unknown escape, unterminated string, unknown leading
/// token, trailing garbage, missing object separators, array/object
/// syntax errors, empty number, and literal typos must all return None.
#[test]
fn json_parser_rejects_malformed_tokens() {
    assert!(parse_json_value(r#""\q""#).is_none()); // unknown escape
    assert!(parse_json_value(r#""abc"#).is_none()); // unterminated string
    assert!(parse_json_value("z").is_none()); // unknown leading token
    assert!(parse_json_value("1 2").is_none()); // trailing garbage after value
    assert!(parse_json_value("{} trailing").is_none());
    assert!(parse_json_value(r#"{"a" 1}"#).is_none()); // missing colon
    assert!(parse_json_value(r#"{"a":1 "b":2}"#).is_none()); // missing comma
    assert!(parse_json_value("[1 2]").is_none()); // array missing comma
    assert!(parse_json_value("-").is_none()); // number without digits
    assert!(parse_json_value("tru").is_none()); // literal typo
    assert!(parse_json_value("nu").is_none()); // null literal typo
    assert!(parse_json_value("fa").is_none()); // false literal typo
    assert!(parse_json_value(r#"{"a":}"#).is_none()); // value missing
}

/// labels_from: keeps only string items, tolerates missing/non-array
/// payloads (forward-compat), and never panics on malformed labels.
#[test]
fn labels_from_string_items_only_and_lenient() {
    let mut body = HashMap::new();
    body.insert(
        "labels".into(),
        JsonValue::Array(vec![
            JsonValue::String("a".into()),
            JsonValue::Number(1),
            JsonValue::String("b".into()),
        ]),
    );
    assert_eq!(labels_from(&body), vec!["a".to_string(), "b".to_string()]);
    // missing labels -> empty
    assert!(labels_from(&HashMap::new()).is_empty());
    // non-array labels -> empty (never a panic)
    let mut m = HashMap::new();
    m.insert("labels".into(), JsonValue::String("not-array".into()));
    assert!(labels_from(&m).is_empty());
    // empty array
    let mut e = HashMap::new();
    e.insert("labels".into(), JsonValue::Array(vec![]));
    assert!(labels_from(&e).is_empty());
}

/// fold derives labels for both issue.created and pr.created events.
#[test]
fn fold_sets_labels_from_created_events() {
    let mk = |kind: EventKind, entity: &str, id: u64, body: HashMap<String, JsonValue>| {
        Event::new_with_id(
            &format!("22222222-2222-4222-8222-{:012x}", id),
            kind,
            entity,
            id,
            "a@x",
            body,
        )
        .unwrap()
    };
    let mut ib = HashMap::new();
    ib.insert("title".into(), JsonValue::String("T".into()));
    ib.insert(
        "labels".into(),
        JsonValue::Array(vec![
            JsonValue::String("bug".into()),
            JsonValue::String("urgent".into()),
        ]),
    );
    let mut pb = HashMap::new();
    pb.insert("description".into(), JsonValue::String("body".into()));
    pb.insert(
        "labels".into(),
        JsonValue::Array(vec![JsonValue::String("enhancement".into())]),
    );
    let st = fold(&[
        mk(EventKind::IssueCreated, "issue", 1, ib),
        mk(EventKind::PrCreated, "pr", 2, pb),
    ]);
    assert_eq!(
        st.issue.labels,
        vec!["bug".to_string(), "urgent".to_string()]
    );
    assert_eq!(st.pr.labels, vec!["enhancement".to_string()]);
    assert_eq!(st.pr.description.as_deref(), Some("body"));
}

/// A JSON `\uXXXX` escape encoding a UTF-16 surrogate pair combines into one
/// supplementary-plane scalar (`\uD83D\uDE00` -> `😀`); lone surrogates and a
/// high surrogate not followed by a low surrogate are rejected.
#[test]
fn json_parser_decodes_surrogate_pairs_and_rejects_lone_surrogates() {
    assert_eq!(
        parse_json_value(r#""\uD83D\uDE00""#).and_then(|v| v.as_str().map(String::from)),
        Some("😀".to_string())
    );
    // lone low surrogate -> None
    assert!(parse_json_value(r#""\uDE00""#).is_none());
    // high surrogate not followed by a low surrogate -> None
    assert!(parse_json_value(r#""\uD83D""#).is_none());
    assert!(parse_json_value(r#""\uD83Dx""#).is_none());
    // high surrogate followed by a non-surrogate -> None
    assert!(parse_json_value(r#""\uD83D\u0041""#).is_none());
    // whitespace between the surrogate escapes is invalid (must be adjacent)
    assert!(parse_json_value(r#""\uD83D \uDE00""#).is_none());
    assert!(parse_json_value("\"\u{8}\\uD83D\t\\uDE00\"").is_none());
}

/// A raw C0 control byte inside a JSON string is invalid JSON (must be
/// escaped); the parser rejects it, while the escaped forms still work.
#[test]
fn json_parser_rejects_raw_c0_in_strings_but_accepts_escapes() {
    assert!(parse_json_value("\"a\u{0001}b\"").is_none()); // raw U+0001
    assert!(parse_json_value("\"a\u{0008}b\"").is_none()); // raw backspace
    assert!(parse_json_value("\"a\nb\"").is_none()); // raw newline
    assert!(parse_json_value("\"a\u{001b}b\"").is_none()); // raw ESC
                                                           // escaped forms are still accepted and decode
    assert_eq!(
        parse_json_value(r#""a\u0001b""#).and_then(|v| v.as_str().map(String::from)),
        Some("a\u{0001}b".to_string())
    );
    assert_eq!(
        parse_json_value(r#""a\nb""#).and_then(|v| v.as_str().map(String::from)),
        Some("a\nb".to_string())
    );
}

/// The CI Check event kind round-trips through the JSON codec: status and plan
/// (body) and the actor (top-level) survive to_json -> from_json verbatim.
#[test]
fn ci_check_event_roundtrips_status_plan_actor() {
    let ev = Event::new_with_id(
        "33333333-3333-4333-8333-000000000001",
        EventKind::CiCheck,
        "pr",
        7,
        "ci@example.com",
        body_with(vec![
            ("status", JsonValue::String("success".into())),
            ("plan", JsonValue::String(".forge/ci.sh".into())),
        ]),
    )
    .unwrap();
    let json = ev.to_json();
    assert!(json.contains("\"kind\":\"ci.check\""), "kind wire: {json}");
    assert!(
        json.contains("\"status\":\"success\""),
        "status wire: {json}"
    );
    assert!(
        json.contains("\"plan\":\".forge/ci.sh\""),
        "plan wire: {json}"
    );
    let round = Event::from_json(&json).expect("ci.check roundtrip must parse");
    assert_eq!(round, ev);
    // The folded PR state surfaces the latest fields.
    let state = fold(&[ev]);
    assert_eq!(state.pr.ci_status.as_deref(), Some("success"));
    assert_eq!(state.pr.ci_plan.as_deref(), Some(".forge/ci.sh"));
    assert_eq!(state.pr.ci_actor.as_deref(), Some("ci@example.com"));
}

/// fold keeps only the LATEST CI Check: a later `ci.check` overwrites the
/// prior status/plan/actor/ts when folded into PR state.
#[test]
fn fold_keeps_latest_ci_check_status() {
    let mk = |id: u64, status: &str| {
        Event::new_with_id(
            &format!("44444444-4444-4444-8444-{:012x}", id),
            EventKind::CiCheck,
            "pr",
            2,
            "ci@example.com",
            body_with(vec![
                ("status", JsonValue::String(status.into())),
                ("plan", JsonValue::String("just check".into())),
            ]),
        )
        .unwrap()
    };
    // success first, then failure — the fold must expose the LATEST (failure).
    let st = fold(&[mk(1, "success"), mk(2, "failed")]);
    assert_eq!(st.pr.ci_status.as_deref(), Some("failed"));
    assert_eq!(st.pr.ci_plan.as_deref(), Some("just check"));
    assert_eq!(st.pr.ci_actor.as_deref(), Some("ci@example.com"));

    // The pending status from `pr create` (t1) is also folded; a later
    // successful run overwrites it.
    let st2 = fold(&[mk(3, "pending"), mk(4, "success")]);
    assert_eq!(st2.pr.ci_status.as_deref(), Some("success"));
}

/// Parse `YYYY-MM-DDTHH:MM:SSZ` (RFC3339 UTC) into seconds since the Unix
/// epoch. The test-time independent oracle for the event timestamp: it is a
/// days-from-civil conversion, deliberately NOT the serialization formatter
/// under test, so a bug that always returns the epoch would break this test.
fn parse_rfc3339_utc(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() != 20
        || b[4] != b'-'
        || b[7] != b'-'
        || b[10] != b'T'
        || b[13] != b':'
        || b[16] != b':'
        || b[19] != b'Z'
    {
        return None;
    }
    let num = |r: std::ops::Range<usize>| -> Option<i64> {
        let mut n = 0i64;
        for &c in &b[r] {
            if !c.is_ascii_digit() {
                return None;
            }
            n = n * 10 + (c - b'0') as i64;
        }
        Some(n)
    };
    let year = num(0..4)?;
    let month = num(5..7)?;
    let day = num(8..10)?;
    let hour = num(11..13)?;
    let min = num(14..16)?;
    let sec = num(17..19)?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || min > 59 || sec > 60 {
        return None;
    }
    // days-from-civil (Howard Hinnant), the inverse of the RFC3339 formatter.
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = if month > 2 { month - 3 } else { month + 9 };
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    Some(days * 86400 + hour * 3600 + min * 60 + sec)
}

/// The RFC3339 UTC formatter is byte-correct at known epochs (independent
/// oracle: a fixed literal, not a recomputation).
#[test]
fn rfc3339_utc_formatter_known_values() {
    assert_eq!(format_rfc3339_utc(0), "1970-01-01T00:00:00Z");
    assert_eq!(format_rfc3339_utc(1_000_000_000), "2001-09-09T01:46:40Z");
    // Round-trips through the independent parser for a mid-range sample.
    for v in [0i64, 1_234_567_890, 2_000_000_000] {
        let s = format_rfc3339_utc(v);
        assert_eq!(parse_rfc3339_utc(&s), Some(v), "roundtrip {s}");
    }
}

/// F-001 deterministic boundary: a wall clock BEFORE the Unix epoch (a reset
/// or misconfigured host clock) must format a real pre-1970 RFC3339 UTC string
/// (negative offset from epoch), never the fabricated `1970-01-01T00:00:00Z`.
/// The fold must carry that real pre-epoch ts into `PrState::ci_ts`.
#[test]
fn pre_epoch_wall_clock_formats_real_time_not_epoch() {
    let t = std::time::UNIX_EPOCH - std::time::Duration::from_secs(1);
    let ts = format_rfc3339_system_time(t);
    assert_eq!(ts, "1969-12-31T23:59:59Z");
    let ev = Event {
        v: 1,
        id: "550e8400-e29b-41d4-a716-446655440000".to_string(),
        kind: EventKind::CiCheck,
        entity: "pr".to_string(),
        entity_id: 1,
        ts,
        actor: "ci@example.com".to_string(),
        body: body_with(vec![
            ("status", JsonValue::String("success".into())),
            ("plan", JsonValue::String("just check".into())),
        ]),
    };
    let state = fold(std::slice::from_ref(&ev));
    assert_eq!(state.pr.ci_ts.as_deref(), Some("1969-12-31T23:59:59Z"));
}

/// F-004: `worktree_registered_path` compares each porcelain `worktree <path>`
/// record EXACTLY, so a registered sibling that shares the owned path's prefix
/// (e.g. `/tmp/owned-other` when `/tmp/owned` was removed) is never mistaken
/// for the owned path — the old substring `contains` check false-matched here.
#[test]
fn worktree_registered_path_exact_not_substring() {
    let porcelain = "worktree /repo\n\
                     HEAD a\n\
                     branch refs/heads/main\n\
                     \n\
                     worktree /repo/owned-other\n\
                     HEAD b\n\
                     detached\n";
    // Owned path was removed; only the prefix-sharing sibling remains.
    assert!(!worktree_registered_path(
        porcelain,
        std::path::Path::new("/repo/owned")
    ));
    // The sibling is exactly registered, so it is found.
    assert!(worktree_registered_path(
        porcelain,
        std::path::Path::new("/repo/owned-other")
    ));
    // A non-prefix path is not reported either.
    assert!(!worktree_registered_path(
        porcelain,
        std::path::Path::new("/repo/none")
    ));

    let with_owned = "worktree /repo\n\
                      HEAD a\n\
                      branch refs/heads/main\n\
                      \n\
                      worktree /repo/owned\n\
                      HEAD c\n\
                      detached\n";
    assert!(worktree_registered_path(
        with_owned,
        std::path::Path::new("/repo/owned")
    ));
}

/// F-005: `worktree_registered_path` canonicalizes both sides before
/// comparing, so a macOS `temp_dir()` spelling (`/var/...`, a symlink to the
/// real `/private/var/...`) still matches the path git actually registered —
/// even after the worktree directory was deleted, when the raw spelling no
/// longer exists and only the nearest existing ancestor can be resolved.
/// Without the canonicalization this stale registration would be missed and
/// the cleanup would report a false success.
#[test]
fn worktree_registered_path_canonicalizes_macos_temp_dir() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir =
        std::env::temp_dir().join(format!("gf-wt-canonical-{}-{}", std::process::id(), nonce));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    // git registers the REAL (symlink-resolved) path; the caller holds the
    // raw `temp_dir()` spelling.
    let real = std::fs::canonicalize(&dir).unwrap();
    // Simulate the directory-deleting removal; the raw path no longer exists.
    std::fs::remove_dir_all(&dir).unwrap();

    let porcelain = format!(
        "worktree {}\nHEAD a\nbranch refs/heads/main\n\ndetached\n",
        real.display()
    );
    assert!(
        worktree_registered_path(&porcelain, &dir),
        "raw temp_dir() spelling must match the real registered path after removal"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// F-001 regression: a CI Check event must carry the actual run timestamp in
/// RFC3339 UTC — the fold's `ci_ts` must expose that same run time, never the
/// hard-coded 1970 epoch placeholder.
#[test]
fn ci_check_event_uses_run_time_timestamp_not_epoch() {
    let ev = Event::new(
        EventKind::CiCheck,
        "pr",
        1,
        "ci@example.com",
        body_with(vec![
            ("status", JsonValue::String("success".into())),
            ("plan", JsonValue::String(".forge/ci.sh".into())),
        ]),
    );
    assert_ne!(
        ev.ts, "1970-01-01T00:00:00Z",
        "CI Check ts must not be the hard-coded epoch placeholder"
    );
    let parsed = parse_rfc3339_utc(&ev.ts).expect("CI Check ts must be RFC3339 UTC");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    assert!(
        (parsed - now).abs() < 60,
        "CI Check ts must be ~the run time (parsed {parsed}, now {now}, ts {})",
        ev.ts
    );
    // The fold exposes the same run time through PrState::ci_ts.
    let state = fold(std::slice::from_ref(&ev));
    assert_eq!(state.pr.ci_ts.as_deref(), Some(ev.ts.as_str()));
}
