use super::*;
use std::error::Error;

#[test]
fn map_repo_open_err_not_found_becomes_not_a_git_repo() {
    // A path that is not a repository yields ErrorCode::NotFound, which
    // must map to the user-facing NotAGitRepository.
    let e = match git2::Repository::open("/tmp/gf-no-such-repo-xyz") {
        Ok(_) => panic!("open must fail on a nonexistent path"),
        Err(e) => e,
    };
    assert!(matches!(
        map_repo_open_err(e),
        StoreError::NotAGitRepository
    ));
}

#[test]
fn store_error_display_constracts_user_facing_text() {
    assert_eq!(
        StoreError::NotAGitRepository.to_string(),
        "not a git repository (or any of the parent directories)"
    );
    assert_eq!(
        StoreError::CasConflict.to_string(),
        "ref CAS conflict — ref moved, retry"
    );
    assert_eq!(
        StoreError::Exhausted.to_string(),
        "bounded CAS retries exhausted; refs unchanged"
    );
    assert_eq!(
        StoreError::MissingEvent.to_string(),
        "entity chain has no event.json payload"
    );
    assert_eq!(
        StoreError::MissingRef.to_string(),
        "required ref does not exist"
    );
    assert_eq!(
        StoreError::RefExists("refs/forge/issues/1".into()).to_string(),
        "entity ref already exists: refs/forge/issues/1"
    );
    assert_eq!(
        StoreError::Command("boom".into()).to_string(),
        "git command failed: boom"
    );
    assert_eq!(
        StoreError::InvalidState("whatev".into()).to_string(),
        "invalid store state: whatev"
    );
}

#[test]
fn store_error_source_exposes_git_but_not_other_variants() {
    // Only the Git variant surfaces a std source; the user-facing variants
    // must report None (they are terminal, not wrapped errors).
    let ge = git2::Error::from_str("boom");
    assert!(StoreError::Git(ge).source().is_some());
    assert!(StoreError::NotAGitRepository.source().is_none());
    assert!(StoreError::CasConflict.source().is_none());
    assert!(StoreError::Exhausted.source().is_none());
    assert!(StoreError::MissingEvent.source().is_none());
    assert!(StoreError::MissingRef.source().is_none());
    assert!(StoreError::RefExists("x".into()).source().is_none());
    assert!(StoreError::Command("y".into()).source().is_none());
    assert!(StoreError::InvalidState("z".into()).source().is_none());
    // Display for the Git wrapper keeps the "git error:" prefix.
    let ge2 = git2::Error::from_str("boom");
    assert!(StoreError::Git(ge2).to_string().starts_with("git error:"));
}

#[test]
fn map_repo_open_err_non_not_found_stays_git() {
    // A git2 error whose code is not NotFound must pass through as a
    // StoreError::Git (keeping its diagnostic), not be coerced to the
    // user-facing "not a git repository" message.
    let ge = git2::Error::from_str("boom");
    assert!(matches!(map_repo_open_err(ge), StoreError::Git(_)));
}

#[test]
fn parse_counter_next_reads_and_rejects() {
    assert_eq!(parse_counter_next(r#"{"v":1,"next":7}"#).unwrap(), 7);
    assert_eq!(parse_counter_next("  {\"v\":1,\"next\":42}  ").unwrap(), 42);
    // missing "next" key
    assert!(matches!(
        parse_counter_next(r#"{"v":1}"#),
        Err(StoreError::InvalidState(_))
    ));
    // non-numeric next
    assert!(matches!(
        parse_counter_next(r#"{"v":1,"next":abc}"#),
        Err(StoreError::InvalidState(_))
    ));
}

#[test]
fn counter_json_accepts_spacing_and_signed_event_range() {
    for content in [
        r#"{"v": 1, "next": 3}"#,
        "\r\n{\t\"next\" \r:\n 3, \"v\" : 1 }\t",
    ] {
        assert_eq!(parse_counter_next(content).unwrap(), 3, "{content}");
    }
    assert_eq!(
        parse_counter_next(r#"{"v":1,"next":9223372036854775807}"#).unwrap(),
        i64::MAX as u64
    );
}

#[test]
fn counter_json_rejects_malformed_documents_and_non_u64_values() {
    for content in [
        r#"{"v":1,"next":3} trailing"#,
        r#"{"v":1,"next":03}"#,
        r#"{"v":1,"next":3.5}"#,
        r#"{"v":1,"next":3e0}"#,
        r#"{"v":1,"next":-3}"#,
        r#"{"v":1,"next":"3"}"#,
        r#"{"v":1,"next":true}"#,
        r#"{"v":1,"next":null}"#,
        r#"{"v":1,"next":0}"#,
        r#"{"v":1,"next":9223372036854775808}"#,
        r#"{"v":1,"next":18446744073709551615}"#,
        r#"{"v":1,"next":18446744073709551616}"#,
        r#"{"v":1,"nested":{"next":3}}"#,
        r#"[{"v":1,"next":3}]"#,
    ] {
        assert!(parse_counter_next(content).is_err(), "accepted {content}");
    }
}

#[test]
fn current_tip_absent_entity_is_none_not_error() {
    // An entity ref that does not exist reads as None (fresh entity), not
    // an error — this is how lazy first allocation starts.
    let d = std::env::temp_dir().join(format!(
        "gf-store-tip-{}-{:?}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .subsec_nanos()
    ));
    let _ = std::fs::remove_dir_all(&d);
    let repo = git2::Repository::init(&d).unwrap();
    let store = EventStore::from_repo(repo).unwrap();
    let tip = store.current_tip("refs/forge/issues/999").unwrap();
    assert_eq!(tip, None);
    let _ = std::fs::remove_dir_all(&d);
}
