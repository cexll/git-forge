//! Declarative issue plans exercised through the real CLI and git refs.
use git_forge::event::EventKind;
use git_forge::store::{issue_ref, EventStore};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Repo(PathBuf);
impl Repo {
    fn new() -> Self {
        let mut n = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = loop {
            let path = std::env::temp_dir().join(format!("gf-apply-{}-{n}", std::process::id()));
            match std::fs::create_dir(&path) {
                Ok(()) => break path,
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => n += 1,
                Err(e) => panic!("{e}"),
            }
        };
        let repo = Self(path);
        repo.git(&["init", "-q"]);
        repo.git(&["config", "user.email", "test@example.com"]);
        repo.git(&["config", "user.name", "Test"]);
        repo
    }
    fn git(&self, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(args)
            .current_dir(&self.0)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }
    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_git-forge"))
            .args(args)
            .current_dir(&self.0)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env_remove("GIT_FORGE_ROLE")
            .output()
            .unwrap()
    }
    fn ok(&self, args: &[&str]) -> String {
        let out = self.run(args);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    }
    fn plan(&self, name: &str, text: &str) -> PathBuf {
        let path = self.0.join(name);
        std::fs::write(&path, text).unwrap();
        path
    }
    fn refs(&self) -> String {
        self.git(&[
            "for-each-ref",
            "--format=%(refname) %(objectname)",
            "refs/forge/",
        ])
    }
}
impl Drop for Repo {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn path_str(path: &Path) -> &str {
    path.to_str().unwrap()
}
fn entry(key: &str, fields: &str, body: &str) -> String {
    format!("---\nkey: {key}\ntitle: {key} title\n{fields}---\n{body}")
}
fn graph(repo: &Repo) -> PathBuf {
    let dir = repo.0.join("plan");
    std::fs::create_dir(&dir).unwrap();
    for (name, key, fields) in [
        ("0-spec.md", "spec", ""),
        ("1-api.md", "api", "parent: spec\n"),
        ("2-schema.md", "schema", "parent: spec\nblocked_by: [api]\n"),
        (
            "3-cli.md",
            "cli",
            "parent: spec\nblocked_by: [api, schema]\n",
        ),
    ] {
        std::fs::write(dir.join(name), entry(key, fields, "Description\n")).unwrap();
    }
    dir
}
#[test]
fn apply_creates_graph_and_dry_run_and_reapply_preserve_refs() {
    let repo = Repo::new();
    let dir = graph(&repo);
    let before = repo.refs();
    let dry = repo.ok(&["issue", "apply", path_str(&dir), "--dry-run", "--json"]);
    assert!(
        dry.contains("\"key\":\"spec\",\"id\":1,\"action\":\"create\""),
        "{dry}"
    );
    assert_eq!(repo.refs(), before);
    let applied = repo.ok(&["issue", "apply", path_str(&dir), "--json"]);
    assert!(applied.contains("\"id\":4"), "{applied}");
    let store = EventStore::open(&repo.0).unwrap();
    assert_eq!(store.counter_next().unwrap(), 5);
    let schema = git_forge::event::fold(&store.read_chain(&issue_ref(3)).unwrap()).issue;
    assert_eq!(schema.title.as_deref(), Some("schema title"));
    let show = repo.ok(&["issue", "show", "3", "--json"]);
    assert!(show.contains("\"parent\":1"), "{show}");
    assert!(show.contains("\"blocked_by\":[2]"), "{show}");
    assert!(show.contains("\"blocked\":true"), "{show}");
    let parent = repo.ok(&["issue", "show", "1", "--json"]);
    assert!(parent.contains("\"children\":[2,3,4]"), "{parent}");
    let refs = repo.refs();
    let again = repo.ok(&["issue", "apply", path_str(&dir), "--json"]);
    assert_eq!(
        again.matches("\"action\":\"unchanged\"").count(),
        4,
        "{again}"
    );
    assert_eq!(repo.refs(), refs);
}

#[test]
fn quoted_keys_resolve_parent_and_blocker_references() {
    for quoted in [r#""project schema,[root]""#, "'   '"] {
        let repo = Repo::new();
        let dir = repo.0.join("plan");
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(
            dir.join("0-root.md"),
            format!("---\nkey: {quoted}\ntitle: Root\n---\n"),
        )
        .unwrap();
        std::fs::write(
            dir.join("1-child.md"),
            entry(
                "child",
                &format!("parent: {quoted}\nblocked_by: [{quoted}]\n"),
                "",
            ),
        )
        .unwrap();
        repo.ok(&["issue", "apply", path_str(&dir), "--json"]);
        let root = repo.ok(&["issue", "show", "1", "--json"]);
        assert!(root.contains("\"children\":[2]"), "{root}");
        let child = repo.ok(&["issue", "show", "2", "--json"]);
        assert!(child.contains("\"parent\":1"), "{child}");
        assert!(child.contains("\"blocked_by\":[1]"), "{child}");
        assert!(child.contains("\"blocked\":true"), "{child}");
        let before = repo.refs();
        let again = repo.ok(&["issue", "apply", path_str(&dir), "--json"]);
        assert_eq!(again.matches("\"action\":\"unchanged\"").count(), 2);
        assert_eq!(repo.refs(), before);
    }
}

#[test]
fn new_description_omission_matches_master_human_output() {
    let repo = Repo::new();
    // Captured from master: omitted and empty positional descriptions are absent.
    for (id, args, expected, description) in [
        (
            1,
            vec!["issue", "new", "Ticket"],
            "#1 Ticket — open\n",
            None,
        ),
        (
            2,
            vec!["issue", "new", "Empty", ""],
            "#2 Empty — open\n",
            None,
        ),
        (
            3,
            vec!["issue", "new", "Detailed", "Body"],
            "#3 Detailed — open\ndescription: Body\n",
            Some("Body"),
        ),
    ] {
        repo.ok(&args);
        assert_eq!(repo.ok(&["issue", "show", &id.to_string()]), expected);
        let store = EventStore::open(&repo.0).unwrap();
        let chain = store.read_chain(&issue_ref(id)).unwrap();
        assert_eq!(
            git_forge::event::fold(&chain).issue.description.as_deref(),
            description
        );
        assert_eq!(
            chain[0].body.contains_key("description"),
            description.is_some()
        );
    }
    let plan = repo.plan("empty-body.md", &entry("empty-body", "", ""));
    repo.ok(&["issue", "apply", path_str(&plan)]);
    let store = EventStore::open(&repo.0).unwrap();
    assert_eq!(
        git_forge::event::fold(&store.read_chain(&issue_ref(4)).unwrap())
            .issue
            .description
            .as_deref(),
        Some("")
    );
}

#[test]
fn metadata_diff_only_appends_updated_and_state_omission_preserves_closed() {
    let repo = Repo::new();
    let dir = graph(&repo);
    repo.ok(&["issue", "apply", path_str(&dir)]);
    repo.ok(&["issue", "close", "3"]);
    std::fs::write(
        dir.join("2-schema.md"),
        entry("schema", "parent: spec\nblocked_by: [cli]\n", "New body\n"),
    )
    .unwrap();
    // Remove cli's dependency on schema so the desired graph stays acyclic.
    std::fs::write(
        dir.join("3-cli.md"),
        entry("cli", "parent: spec\nblocked_by: [api]\n", "Description\n"),
    )
    .unwrap();
    let result = repo.ok(&["issue", "apply", path_str(&dir), "--json"]);
    assert!(
        result.contains("\"changed_fields\":[\"description\",\"blocked_by\"]"),
        "{result}"
    );
    let store = EventStore::open(&repo.0).unwrap();
    let chain = store.read_chain(&issue_ref(3)).unwrap();
    assert_eq!(
        chain.iter().map(|e| e.kind).collect::<Vec<_>>(),
        vec![
            EventKind::IssueCreated,
            EventKind::IssueClose,
            EventKind::IssueUpdated
        ]
    );
    let event = chain.last().unwrap();
    assert_eq!(event.body.len(), 2);
    let state = git_forge::event::fold(&chain).issue;
    assert!(!state.open);
    assert_eq!(state.description.as_deref(), Some("New body\n"));
    assert_eq!(state.blocked_by, vec![4]);
}

#[test]
fn invalid_plans_name_file_and_field_and_write_nothing() {
    for (field, text) in [
        ("key", "---\nkey: \"\"\ntitle: Root\n---\n".into()),
        ("key", "---\nkey: \"#reserved\"\ntitle: Root\n---\n".into()),
        ("title", "---\nkey: one\ntitle: '   '\n---\n".into()),
        ("blocked_by", entry("one", "blocked_by: [one]\n", "")),
        ("blocked_by", entry("one", "blocked_by: [missing]\n", "")),
        ("parent", entry("one", "parent: one\n", "")),
        ("parent", entry("one", "parent: missing\n", "")),
        ("title", "---\nkey: one\ntitle: \n---\n".into()),
        (
            "title",
            "---\nkey: one\ntitle: first\ntitle: second\n---\n".into(),
        ),
        ("state", entry("one", "state: pending\n", "")),
        ("labels", entry("one", "labels: [a,]\n", "")),
        ("unknown", entry("one", "unknown: value\n", "")),
    ] {
        let repo = Repo::new();
        let path = repo.plan("bad.md", &text);
        let before = repo.refs();
        let out = repo.run(&["issue", "apply", path_str(&path)]);
        assert!(!out.status.success());
        let error = String::from_utf8(out.stderr).unwrap();
        assert!(error.contains("bad.md") && error.contains(field), "{error}");
        assert_eq!(repo.refs(), before);
    }
}

#[test]
fn duplicate_keys_and_multi_issue_cycles_abort_the_whole_plan() {
    for (second_key, first_fields, second_fields, field) in [
        ("one", "", "", "key"),
        (
            "two",
            "blocked_by: [two]\n",
            "blocked_by: [one]\n",
            "blocked_by",
        ),
        ("two", "parent: two\n", "parent: one\n", "parent"),
    ] {
        let repo = Repo::new();
        let dir = repo.0.join("plan");
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join("one.md"), entry("one", first_fields, "")).unwrap();
        std::fs::write(dir.join("two.md"), entry(second_key, second_fields, "")).unwrap();
        let before = repo.refs();
        let out = repo.run(&["issue", "apply", path_str(&dir)]);
        assert!(!out.status.success());
        assert!(String::from_utf8_lossy(&out.stderr).contains(field));
        assert_eq!(repo.refs(), before);
    }
}

#[test]
fn existing_references_filters_and_blocked_follow_live_state() {
    let repo = Repo::new();
    repo.ok(&["issue", "new", "Existing"]);
    let path = repo.plan(
        "child.md",
        &entry(
            "child",
            "parent: \"#1\"\nblocked_by: [\"#1\"]\nlabels: [ready-for-agent]\n",
            "Body\n",
        ),
    );
    repo.ok(&["issue", "apply", path_str(&path)]);
    let list = repo.ok(&[
        "issue", "list", "--parent", "1", "--state", "open", "--json",
    ]);
    assert!(
        list.contains("\"id\":2") && !list.contains("\"id\":1"),
        "{list}"
    );
    assert!(list.contains("\"blocked\":true"), "{list}");
    repo.ok(&["issue", "close", "1"]);
    let show = repo.ok(&["issue", "show", "2", "--json"]);
    assert!(show.contains("\"blocked\":false"), "{show}");
    let closed = repo.ok(&["issue", "list", "--state", "closed", "--json"]);
    assert!(
        closed.contains("\"id\":1") && !closed.contains("\"id\":2"),
        "{closed}"
    );
}

#[test]
fn new_graph_options_share_validation_and_reject_missing_dependencies() {
    let repo = Repo::new();
    repo.ok(&["issue", "new", "Spec"]);
    repo.ok(&["issue", "new", "API", "--parent", "1"]);
    let new = repo.ok(&[
        "issue",
        "new",
        "CLI",
        "--parent",
        "1",
        "--blocked-by",
        "1",
        "--blocked-by",
        "2",
        "--json",
    ]);
    assert!(
        new.contains("\"parent\":1") && new.contains("\"blocked_by\":[1,2]"),
        "{new}"
    );
    let before = repo.refs();
    for args in [
        vec!["issue", "new", "Bad", "--parent", "99"],
        vec!["issue", "new", "Bad", "--blocked-by", "99"],
    ] {
        assert!(!repo.run(&args).status.success());
        assert_eq!(repo.refs(), before);
    }
}

#[test]
fn cycles_through_existing_issues_are_rejected_without_writes() {
    let repo = Repo::new();
    let a = repo.plan("a.md", &entry("a", "", ""));
    repo.ok(&["issue", "apply", path_str(&a)]);
    let b = repo.plan(
        "b.md",
        &entry("b", "parent: \"#1\"\nblocked_by: [\"#1\"]\n", ""),
    );
    repo.ok(&["issue", "apply", path_str(&b)]);
    for fields in ["parent: \"#2\"\n", "blocked_by: [\"#2\"]\n"] {
        std::fs::write(&a, entry("a", fields, "")).unwrap();
        let before = repo.refs();
        let out = repo.run(&["issue", "apply", path_str(&a)]);
        assert!(!out.status.success());
        assert!(String::from_utf8_lossy(&out.stderr).contains("cycle"));
        assert_eq!(repo.refs(), before);
    }
}

#[test]
fn explicit_state_and_metadata_clearing_are_idempotent() {
    let repo = Repo::new();
    repo.ok(&["issue", "new", "External"]);
    let path = repo.plan(
        "a.md",
        &entry(
            "a",
            "parent: \"#1\"\nblocked_by: [\"#1\"]\nlabels: [agent]\nstate: closed\n",
            "",
        ),
    );
    repo.ok(&["issue", "apply", path_str(&path)]);
    std::fs::write(&path, entry("a", "state: open\n", "")).unwrap();
    repo.ok(&["issue", "apply", path_str(&path)]);
    let show = repo.ok(&["issue", "show", "2", "--json"]);
    for expected in [
        "\"parent\":null",
        "\"blocked_by\":[]",
        "\"labels\":[]",
        "\"open\":true",
    ] {
        assert!(show.contains(expected), "{show}");
    }
    let before = repo.refs();
    repo.ok(&["issue", "apply", path_str(&path)]);
    assert_eq!(repo.refs(), before);
}

#[test]
fn new_refuses_stale_counter_collision_instead_of_replacing_issue() {
    let repo = Repo::new();
    repo.ok(&["issue", "new", "First"]);
    let stale = repo.git(&["rev-parse", "refs/forge/meta/counter"]);
    repo.ok(&["issue", "new", "Second"]);
    repo.git(&["update-ref", "refs/forge/meta/counter", stale.trim()]);
    let before = repo.refs();
    let out = repo.run(&["issue", "new", "Replacement"]);
    assert!(
        !out.status.success(),
        "must reject an occupied allocated id"
    );
    assert_eq!(repo.refs(), before);
    let show = repo.ok(&["issue", "show", "2", "--json"]);
    assert!(show.contains("Second"), "{show}");
}

#[test]
fn directory_plan_ignores_symlinks_and_nested_entries() {
    let repo = Repo::new();
    let dir = graph(&repo);
    std::os::unix::fs::symlink("missing.md", dir.join("link.md")).unwrap();
    std::fs::create_dir(dir.join("nested")).unwrap();
    std::fs::write(dir.join("nested/bad.md"), "not a plan").unwrap();
    let result = repo.ok(&["issue", "apply", path_str(&dir), "--json"]);
    assert_eq!(result.matches("\"action\":\"create\"").count(), 4);
}

#[test]
fn bare_existing_blocker_reference_is_rejected_without_writes() {
    let repo = Repo::new();
    repo.ok(&["issue", "new", "Existing"]);
    let path = repo.plan("bad.md", &entry("child", "blocked_by: [#1]\n", ""));
    let before = repo.refs();
    let out = repo.run(&["issue", "apply", path_str(&path)]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("blocked_by"));
    assert_eq!(repo.refs(), before);
}

#[test]
fn plan_errors_escape_terminal_controls_in_fields_references_and_paths() {
    let repo = Repo::new();
    for (name, text) in [
        ("field.md", entry("a", "\u{1b}]52;c;YQ==\u{7}: value\n", "")),
        (
            "reference.md",
            entry("a", "blocked_by: [\"\\u001b[31mmissing\"]\n", ""),
        ),
        ("\u{1b}[31mpath.md", entry("a", "unknown: value\n", "")),
    ] {
        let path = repo.plan(name, &text);
        let out = repo.run(&["issue", "apply", path_str(&path)]);
        assert!(!out.status.success());
        let error = String::from_utf8(out.stderr).unwrap();
        assert!(
            !error.contains('\u{1b}') && !error.contains('\u{7}'),
            "{error:?}"
        );
        assert!(error.contains("\\x1b"), "{error}");
        assert_eq!(repo.refs(), "");
    }
}

#[test]
fn new_retries_fresh_snapshot_after_competing_allocation() {
    use std::os::unix::fs::PermissionsExt;
    for initialized in [false, true] {
        let repo = Repo::new();
        if initialized {
            repo.ok(&["issue", "new", "Initial"]);
        }
        let expected_counter = if initialized {
            repo.git(&["rev-parse", "refs/forge/meta/counter"])
                .trim()
                .to_owned()
        } else {
            String::new()
        };
        let shims = repo.0.join("shims");
        std::fs::create_dir(&shims).unwrap();
        let real_git = repo.git(&["--exec-path"]);
        let real_git = Path::new(real_git.trim()).join("git");
        let real_git = if real_git.is_file() {
            real_git
        } else {
            PathBuf::from("/usr/bin/git")
        };
        let marker = shims.join("competed");
        let rejection = shims.join("rejection");
        let script = format!("#!/bin/bash\nif [[ \"$*\" == *'update-ref --stdin'* && ! -e '{}' ]]; then\n  touch '{}'\n  '{}' issue new Competitor || exit $?\n  '{}' \"$@\" 2>'{}'\n  code=$?\n  cat '{}' >&2\n  exit $code\nfi\nexec '{}' \"$@\"\n", marker.display(), marker.display(), env!("CARGO_BIN_EXE_git-forge"), real_git.display(), rejection.display(), rejection.display(), real_git.display());
        let shim = shims.join("git");
        std::fs::write(&shim, script).unwrap();
        std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();
        let output = Command::new(env!("CARGO_BIN_EXE_git-forge"))
            .args(["issue", "new", "Requested", "--json"])
            .current_dir(&repo.0)
            .env(
                "PATH",
                format!("{}:{}", shims.display(), std::env::var("PATH").unwrap()),
            )
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env_remove("GIT_FORGE_ROLE")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(marker.exists(), "the competing allocation must execute");
        let diagnostic = std::fs::read_to_string(&rejection).unwrap();
        if initialized {
            assert!(
                diagnostic.contains("cannot lock ref 'refs/forge/meta/counter': is at "),
                "{diagnostic}"
            );
            assert!(
                diagnostic.contains(&format!(" but expected {expected_counter}")),
                "{diagnostic}"
            );
        } else {
            assert!(
                diagnostic.contains("reference already exists"),
                "{diagnostic}"
            );
        }
        let competitor = if initialized { 2 } else { 1 };
        let store = EventStore::open(&repo.0).unwrap();
        assert_eq!(store.counter_next().unwrap(), competitor + 2);
        assert_eq!(
            git_forge::event::fold(&store.read_chain(&issue_ref(competitor)).unwrap())
                .issue
                .title
                .as_deref(),
            Some("Competitor")
        );
        assert_eq!(
            git_forge::event::fold(&store.read_chain(&issue_ref(competitor + 1)).unwrap())
                .issue
                .title
                .as_deref(),
            Some("Requested")
        );
    }
}

#[test]
fn new_does_not_retry_nonconflict_transaction_failure() {
    use std::os::unix::fs::PermissionsExt;
    let repo = Repo::new();
    let shims = repo.0.join("shims");
    std::fs::create_dir(&shims).unwrap();
    let attempts = shims.join("attempts");
    let shim = shims.join("git");
    std::fs::write(&shim, format!("#!/bin/bash\nif [[ \"$*\" == *'update-ref --stdin'* ]]; then\n  echo attempted >> '{}'\n  echo 'injected write failure' >&2\n  exit 73\nfi\nexec /usr/bin/git \"$@\"\n", attempts.display())).unwrap();
    std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_git-forge"))
        .args(["issue", "new", "Requested"])
        .current_dir(&repo.0)
        .env(
            "PATH",
            format!("{}:{}", shims.display(), std::env::var("PATH").unwrap()),
        )
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env_remove("GIT_FORGE_ROLE")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("injected write failure"));
    assert_eq!(std::fs::read_to_string(attempts).unwrap(), "attempted\n");
    assert_eq!(repo.refs(), "");
}

#[test]
fn new_propagates_noncas_failure_despite_concurrent_counter_movement() {
    use std::os::unix::fs::PermissionsExt;
    let repo = Repo::new();
    let shims = repo.0.join("shims");
    std::fs::create_dir(&shims).unwrap();
    let attempts = shims.join("attempts");
    let shim = shims.join("git");
    std::fs::write(
        &shim,
        format!(
            "#!/bin/bash\nif [[ \"$*\" == *'update-ref --stdin'* ]]; then\n  echo attempted >> '{}'\n  if [[ ! -e '{}'/competed ]]; then\n    touch '{}'/competed\n    PATH=/usr/bin:/bin '{}' issue new Competitor || exit $?\n    echo 'injected non-CAS failure' >&2\n    exit 73\n  fi\nfi\nexec /usr/bin/git \"$@\"\n",
            attempts.display(),
            shims.display(),
            shims.display(),
            env!("CARGO_BIN_EXE_git-forge"),
        ),
    )
    .unwrap();
    std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_git-forge"))
        .args(["issue", "new", "Requested"])
        .current_dir(&repo.0)
        .env(
            "PATH",
            format!("{}:{}", shims.display(), std::env::var("PATH").unwrap()),
        )
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env_remove("GIT_FORGE_ROLE")
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "non-CAS failure was hidden: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("injected non-CAS failure"));
    assert_eq!(std::fs::read_to_string(attempts).unwrap(), "attempted\n");
    let store = EventStore::open(&repo.0).unwrap();
    assert_eq!(store.counter_next().unwrap(), 2);
    assert_eq!(
        git_forge::event::fold(&store.read_chain(&issue_ref(1)).unwrap())
            .issue
            .title
            .as_deref(),
        Some("Competitor")
    );
    assert!(store.repo().find_reference(&issue_ref(2)).is_err());
}

#[test]
fn cycle_diagnostic_names_changed_entry_not_unrelated_first_file() {
    let repo = Repo::new();
    let a = repo.plan("a.md", &entry("a", "", ""));
    repo.ok(&["issue", "apply", path_str(&a)]);
    let b = repo.plan("b.md", &entry("b", "", ""));
    repo.ok(&["issue", "apply", path_str(&b)]);
    std::fs::write(&a, entry("a", "blocked_by: [\"#2\"]\n", "")).unwrap();
    repo.ok(&["issue", "apply", path_str(&a)]);
    let dir = repo.0.join("change");
    std::fs::create_dir(&dir).unwrap();
    std::fs::write(dir.join("0-unrelated.md"), entry("unrelated", "", "")).unwrap();
    std::fs::write(
        dir.join("1-offending.md"),
        entry("b", "blocked_by: [\"#1\"]\n", ""),
    )
    .unwrap();
    let before = repo.refs();
    let output = repo.run(&["issue", "apply", path_str(&dir)]);
    assert!(!output.status.success());
    let error = String::from_utf8(output.stderr).unwrap();
    assert!(error.contains("1-offending.md: blocked_by:"), "{error}");
    assert_eq!(repo.refs(), before);
}

#[test]
fn cycle_errors_name_changed_edge_not_unchanged_ancestor() {
    for field in ["parent", "blocked_by"] {
        let repo = Repo::new();
        let relation = |id: u64| {
            if field == "parent" {
                format!("parent: \"#{id}\"\n")
            } else {
                format!("blocked_by: [\"#{id}\"]\n")
            }
        };
        let b = repo.plan("b.md", &entry("b", "", ""));
        repo.ok(&["issue", "apply", path_str(&b)]);
        let a = repo.plan("a.md", &entry("a", &relation(1), ""));
        repo.ok(&["issue", "apply", path_str(&a)]);
        let c = repo.plan("c.md", &entry("c", &relation(2), ""));
        repo.ok(&["issue", "apply", path_str(&c)]);
        let dir = repo.0.join("change");
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join("0-unchanged.md"), entry("c", &relation(2), "")).unwrap();
        std::fs::write(dir.join("1-offending.md"), entry("b", &relation(2), "")).unwrap();
        let before = repo.refs();
        let output = repo.run(&["issue", "apply", path_str(&dir)]);
        assert!(!output.status.success());
        let error = String::from_utf8(output.stderr).unwrap();
        assert!(
            error.contains(&format!("1-offending.md: {field}:")),
            "{error}"
        );
        assert_eq!(repo.refs(), before);
    }
}
