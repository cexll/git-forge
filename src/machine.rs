//! Machine interface (#7, t7): `--json` output rendering and the `version`
//! probe. Split from `cli.rs` so the CLI layer stays under the size gate.
//!
//! Every JSON document is rendered by ONE hand-ordered template function per
//! command: field order and key sets are fixed by the template, not by a map
//! iteration, so output is byte-stable by construction (VAL-201). Values are
//! escaped with the event-wire serializer's [`json_escape`] (the single escape
//! implementation for machine-readable forge output). Prose output stays the
//! parallel human surface; `--json` never replaces it.
//!
//! Fixtures: `tests/fixtures/json/<command>.json` are byte-compared by the
//! t7 tests; a schema change must regenerate them in the same commit.

use crate::cli_error::CliError;

use crate::event::fold;
use crate::store::EventStore;

/// Escape a string for embedding in a rendered JSON document. The machine
/// surface delegates to the event-wire string serializer (`json_escape_for_machine`)
/// so the wire and `--json` escaping can never drift.
fn json_escape(s: &str) -> String {
    crate::event::json_escape_for_machine(s)
}

/// [`json_requested_with`] for a grammar without value-taking options.
pub(crate) fn json_requested(args: &[String]) -> (bool, Vec<String>) {
    json_requested_with(args, &[])
}

thread_local! {
    /// The output mode the last grammar-aware split decided (see
    /// [`json_mode`]).
    static JSON_MODE: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
}

/// The output mode the command's grammar decided: whether a standalone
/// `--json` was present. `None` before any command grammar ran (top-level
/// dispatch errors), where the caller falls back to any `--json` token.
pub fn json_mode() -> Option<bool> {
    JSON_MODE.with(std::cell::Cell::get)
}

/// The namespace dispatcher's split (`issue`, `pr`, `ci`): `argv` is the
/// namespace's arguments with the subcommand first, `values` the
/// subcommand's value-taking options (none for an unknown subcommand, so
/// every `--json` after it is standalone). A `--json` in the subcommand slot
/// is standalone too: the dispatcher rejects it as an unknown subcommand,
/// and that error is JSON.
pub(crate) fn json_requested_by_dispatch(argv: &[String], values: &[&str]) -> (bool, Vec<String>) {
    let (json, rest) = json_requested_with(argv.get(1..).unwrap_or(&[]), values);
    let json = json || argv.first().is_some_and(|sub| sub == "--json");
    JSON_MODE.with(|mode| mode.set(Some(json)));
    (json, rest)
}

/// Split standalone `--json` tokens from `args`: whether at least one was
/// present, plus the remaining arguments in order. `values` names the
/// command's value-taking options: each such option and the token after it
/// are copied verbatim, so an option value that equals `--json` is data
/// (the same rule as an option value equal to `-h`/`--help`). A `"--"` entry
/// declares end-of-options: from a `--` token on, every token is data.
/// A standalone `--json` is recognized in ANY other position; `--json=x` is
/// NOT matched. The decision is recorded for [`json_mode`], so a failure
/// renders as JSON only when the grammar saw a standalone `--json`.
/// Grammaticality of the leftovers belongs to each per-command parser, which
/// rejects any token it does not know (exit 1, VAL-202).
pub(crate) fn json_requested_with(args: &[String], values: &[&str]) -> (bool, Vec<String>) {
    let mut json = false;
    let mut rest = Vec::with_capacity(args.len());
    let mut tokens = args.iter();
    while let Some(a) = tokens.next() {
        if a == "--" && values.contains(&"--") {
            rest.push(a.clone());
            rest.extend(tokens.by_ref().cloned());
        } else if a == "--json" {
            json = true;
        } else {
            rest.push(a.clone());
            if values.contains(&a.as_str()) {
                rest.extend(tokens.next().cloned());
            }
        }
    }
    JSON_MODE.with(|mode| mode.set(Some(json)));
    (json, rest)
}

/// Reject any FLAG token among the leftovers of a command whose grammar
/// takes no options: a usage error naming the flag (`--json=x` gets the
/// bare-flag hint), exit-1, before any side effect (VAL-202). Positionals are
/// NOT judged here — grammars that consume them (`<n> <body>`) validate
/// their own arity/existence. Shared by the pure-positional surfaces
/// (`issue list/show/close/reopen`, `pr list/show/diff`) and both comment
/// parsers (flags only) so an unknown flag can never be silently ignored.
pub(crate) fn reject_flags(args: &[String], usage: &str) -> Result<(), CliError> {
    for a in args {
        if a.starts_with('-') {
            return Err(CliError::usage(
                if a == "--json=x" {
                    format!("unknown option '{a}' (the machine flag is bare `--json`); {usage}")
                } else {
                    format!("unknown option '{a}'; {usage}")
                },
                a,
            ));
        }
    }
    Ok(())
}

/// [`reject_flags`] plus an unexpected-positional refusal for grammars with
/// NO positionals at all (`issue list`, `pr list`): anything left over after
/// the flag check is a `too many` usage error.
pub(crate) fn reject_leftover_flags(args: &[String], usage: &str) -> Result<(), CliError> {
    reject_flags(args, usage)?;
    if !args.is_empty() {
        return Err(CliError::usage(
            format!("{usage} (got {} unexpected argument(s))", args.len()),
            "",
        ));
    }
    Ok(())
}

/// Render an `Option<&str>` JSON string member: `null` when absent (and for an
/// empty string in nullable free-text positions — the machine surface speaks
/// presence, not emptiness).
fn opt_str(s: Option<&str>) -> String {
    match s {
        Some(v) if !v.is_empty() => json_escape(v),
        _ => "null".to_string(),
    }
}

/// Render a number member.
fn num(n: u64) -> String {
    n.to_string()
}

/// Render a string-array member.
fn str_array(items: &[String]) -> String {
    let mut out = String::from("[");
    let mut first = true;
    for i in items {
        if !first {
            out.push(',');
        }
        first = false;
        out.push_str(&json_escape(i));
    }
    out.push(']');
    out
}

// ─────────────────────────── issue schemas ───────────────────────────

fn id_array(ids: impl Iterator<Item = u64>) -> String {
    let mut out = String::from("[");
    for (index, id) in ids.enumerate() {
        if index != 0 {
            out.push(',');
        }
        out.push_str(&num(id));
    }
    out.push(']');
    out
}

fn issue_blocked(state: &crate::event::IssueState, all: &[crate::event::IssueState]) -> bool {
    state
        .blocked_by
        .iter()
        .any(|id| all.iter().any(|issue| issue.id == *id && issue.open))
}

/// `issue new --json` / `issue show --json` result, with derived graph fields.
pub(crate) fn issue_json(
    state: &crate::event::IssueState,
    all: &[crate::event::IssueState],
) -> String {
    format!(
        "{{\"id\":{},\"title\":{},\"description\":{},\"labels\":{},\"open\":{},\"comments\":{},\"key\":{},\"parent\":{},\"blocked_by\":{},\"children\":{},\"blocked\":{}}}",
        num(state.id),
        opt_str(state.title.as_deref()),
        opt_str(state.description.as_deref()),
        str_array(&state.labels),
        state.open,
        str_array(&state.comments),
        opt_str(state.key.as_deref()),
        state.parent.map(num).unwrap_or_else(|| "null".into()),
        id_array(state.blocked_by.iter().copied()),
        id_array(all.iter().filter(|issue| issue.parent == Some(state.id)).map(|issue| issue.id)),
        issue_blocked(state, all),
    )
}

/// `issue list --json` result in id order; blockers derive from the unfiltered graph.
pub(crate) fn issue_list_json(
    states: &[&crate::event::IssueState],
    all: &[crate::event::IssueState],
) -> String {
    let open_ids: std::collections::HashSet<_> = all
        .iter()
        .filter(|issue| issue.open)
        .map(|issue| issue.id)
        .collect();
    let mut out = String::from("[");
    for (index, state) in states.iter().enumerate() {
        if index != 0 {
            out.push(',');
        }
        out.push_str(&format!(
            "{{\"id\":{},\"title\":{},\"open\":{},\"key\":{},\"parent\":{},\"blocked_by\":{},\"blocked\":{}}}",
            num(state.id),
            opt_str(state.title.as_deref()),
            state.open,
            opt_str(state.key.as_deref()),
            state.parent.map(num).unwrap_or_else(|| "null".into()),
            id_array(state.blocked_by.iter().copied()),
            state.blocked_by.iter().any(|id| open_ids.contains(id)),
        ));
    }
    out.push(']');
    out
}

// ─────────────────────────── pr schemas ───────────────────────────

/// `ci` member of the pr schemas: `{"status":..,"plan":..,"reason":..,"tail":..}`
/// or `null`. `reason` and `tail` are `null` unless the latest Check failed
/// with a recorded reason; a recorded empty tail stays `""`.
fn ci_json(state: &crate::event::PrState) -> String {
    match state.ci_status.as_deref() {
        Some(s) => format!(
            "{{\"status\":{},\"plan\":{},\"reason\":{},\"tail\":{}}}",
            json_escape(s),
            opt_str(state.ci_plan.as_deref()),
            opt_str(state.ci_reason.as_deref()),
            state
                .ci_tail
                .as_deref()
                .map_or_else(|| "null".to_string(), json_escape),
        ),
        None => "null".to_string(),
    }
}

/// The full PR object (pr create/show). Snapshot OIDs come from the event
/// fold (the authoritative snapshot), never re-resolved from live branches.
pub(crate) fn pr_json(state: &crate::event::PrState) -> String {
    let prepared = match &state.prepared {
        Some(c) => format!(
            "{{\"strategy\":{},\"result_commit\":{},\"result_tree\":{},\"generation\":{},\"base_head\":{}}}",
            json_escape(&c.strategy),
            json_escape(&c.result_commit),
            json_escape(&c.result_tree),
            num(c.generation),
            json_escape(&c.base_head),
        ),
        None => "null".to_string(),
    };
    format!(
        "{{\"id\":{},\"title\":{},\"description\":{},\"labels\":{},\"source\":{},\"base\":{},\"source_head\":{},\"base_head\":{},\"merge_base\":{},\"closes\":{},\"decision\":{},\"decision_role\":{},\"merged\":{},\"abandoned\":{},\"ci\":{},\"generation\":{},\"prepared\":{},\"creator_role\":{}}}",
        num(state.id),
        opt_str(state.title.as_deref()),
        opt_str(state.description.as_deref()),
        str_array(&state.labels),
        opt_str(state.source_ref.as_deref()),
        opt_str(state.base_ref.as_deref()),
        opt_str(state.source_head.as_deref()),
        opt_str(state.base_head.as_deref()),
        opt_str(state.merge_base.as_deref()),
        match state.closes {
            Some(c) => num(c),
            None => "null".to_string(),
        },
        opt_str(state.effective_decision.as_deref()),
        opt_str(state.decision_role.as_deref()),
        opt_str(state.merge_result.as_deref()),
        opt_str(state.abandoned.as_deref()),
        ci_json(state),
        num(state.generation),
        prepared,
        opt_str(state.creator_role.as_deref()),
    )
}

/// `pr list --json` result: the compact projection (id/title/decision) the
/// prose list renders, in id order.
pub(crate) fn pr_list_json(states: &[crate::event::PrState]) -> String {
    let mut out = String::from("[");
    let mut first = true;
    for s in states {
        if !first {
            out.push(',');
        }
        first = false;
        out.push_str(&format!(
            "{{\"id\":{},\"title\":{},\"decision\":{}}}",
            num(s.id),
            opt_str(s.title.as_deref()),
            opt_str(s.effective_decision.as_deref()),
        ));
    }
    out.push(']');
    out
}

/// `ci run --json` success result: the recorded Check's outcome.
pub(crate) fn ci_run_json(id: u64, status: &str, plan: &str, generation: u64) -> String {
    format!(
        "{{\"id\":{},\"status\":{},\"plan\":{},\"generation\":{}}}",
        num(id),
        json_escape(status),
        json_escape(plan),
        num(generation),
    )
}

// ─────────────────────────── mutation results ───────────────────────────

/// Opening members shared by every mutation result: entity id and the state
/// the entity is in after the command (`open|closed` for Issues,
/// `open|merged|abandoned` for PRs).
fn result_head(id: u64, state: &str) -> String {
    format!("{{\"id\":{},\"state\":{}", num(id), json_escape(state))
}

/// Lifecycle state of a folded Issue.
pub(crate) fn issue_state_name(state: &crate::event::IssueState) -> &'static str {
    if state.open {
        "open"
    } else {
        "closed"
    }
}

/// Lifecycle state of a folded PR: merged and abandoned are terminal.
pub(crate) fn pr_state_name(state: &crate::event::PrState) -> &'static str {
    if state.merge_result.is_some() {
        "merged"
    } else if state.abandoned.is_some() {
        "abandoned"
    } else {
        "open"
    }
}

/// Lifecycle state of the Issue folded as of `event`, the event commit a
/// mutation appended. An append can re-parent onto a newer tip, so the state
/// read before the append can be stale; this is the state the write produced.
pub(crate) fn issue_state_at(
    store: &EventStore,
    event: git2::Oid,
) -> Result<&'static str, CliError> {
    let chain = store.read_chain_at(event).map_err(CliError::from)?;
    Ok(issue_state_name(&fold(&chain).issue))
}

/// Lifecycle state of the PR folded as of `event` (see [`issue_state_at`]).
pub(crate) fn pr_state_at(store: &EventStore, event: git2::Oid) -> Result<&'static str, CliError> {
    let chain = store.read_chain_at(event).map_err(CliError::from)?;
    Ok(pr_state_name(&fold(&chain).pr))
}

/// `issue comment|close|reopen --json` and `pr comment --json`: the entity,
/// its resulting state, and the appended event commit.
pub(crate) fn event_result_json(id: u64, state: &str, event: &str) -> String {
    format!(
        "{},\"event\":{}}}",
        result_head(id, state),
        json_escape(event)
    )
}

/// `pr abandon --json`: the PR, its state after the abandon event (`merged`
/// when a merge landed first), the recorded reason and the event.
pub(crate) fn pr_abandon_json(id: u64, state: &str, reason: &str, event: &str) -> String {
    format!(
        "{},\"reason\":{},\"event\":{}}}",
        result_head(id, state),
        json_escape(reason),
        json_escape(event)
    )
}

/// `pr review --json`: the recorded decision and the generation and prepared
/// candidate (`null` when none was current) it binds.
pub(crate) fn pr_review_json(
    id: u64,
    state: &str,
    decision: &str,
    generation: u64,
    candidate: Option<&str>,
    event: &str,
) -> String {
    format!(
        "{},\"decision\":{},\"generation\":{},\"candidate\":{},\"event\":{}}}",
        result_head(id, state),
        json_escape(decision),
        num(generation),
        opt_str(candidate),
        json_escape(event)
    )
}

/// `pr prepare --json`: the pinned candidate (`candidate` = result commit)
/// with the generation and live base tip it was materialized against.
#[allow(clippy::too_many_arguments)]
pub(crate) fn pr_prepare_json(
    id: u64,
    state: &str,
    strategy: &str,
    generation: u64,
    candidate: &str,
    result_tree: &str,
    base_head: &str,
    event: &str,
) -> String {
    format!(
        "{},\"strategy\":{},\"generation\":{},\"candidate\":{},\"result_tree\":{},\"base_head\":{},\"event\":{}}}",
        result_head(id, state),
        json_escape(strategy),
        num(generation),
        json_escape(candidate),
        json_escape(result_tree),
        json_escape(base_head),
        json_escape(event)
    )
}

/// `pr update --json`: the new snapshot generation, its pinned commits, and
/// the appended event.
pub(crate) fn pr_update_json(
    id: u64,
    state: &str,
    generation: u64,
    source_head: &str,
    base_head: &str,
    merge_base: &str,
    event: &str,
) -> String {
    format!(
        "{},\"generation\":{},\"source_head\":{},\"base_head\":{},\"merge_base\":{},\"event\":{}}}",
        result_head(id, state),
        num(generation),
        json_escape(source_head),
        json_escape(base_head),
        json_escape(merge_base),
        json_escape(event)
    )
}

/// `pr diff --json`: the three-dot diff text between the current
/// generation's snapshot `base` and `head` commits.
pub(crate) fn pr_diff_json(
    state: &crate::event::PrState,
    base: &str,
    head: &str,
    diff: &str,
) -> String {
    format!(
        "{},\"generation\":{},\"base\":{},\"head\":{},\"diff\":{}}}",
        result_head(state.id, pr_state_name(state)),
        num(state.generation),
        json_escape(base),
        json_escape(head),
        json_escape(diff)
    )
}

/// `pr merge --json`: the merged PR, the strategy and base it merged into,
/// the snapshot generation, the new base tip, and the recycle outcome or null.
pub(crate) fn pr_merge_json(outcome: &crate::merge_state::MergeOutcome) -> String {
    format!(
        "{},\"strategy\":{},\"base\":{},\"generation\":{},\"merge_commit\":{},\"recycle\":{}}}",
        result_head(outcome.id, outcome.state),
        json_escape(&outcome.strategy),
        json_escape(&outcome.base_ref),
        num(outcome.generation),
        json_escape(&outcome.merge_commit),
        opt_str(outcome.recycle.as_deref())
    )
}

// ─────────────────────────── version ───────────────────────────

/// `git forge version` — the semver probe the adapter version-gates on
/// (capabilities discovery itself is DEFERRED to a later issue). Prints the
/// crate version (`CARGO_PKG_VERSION`): one bare semver line, exit 0.
pub fn version_string() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

/// `git forge version` argument handling: no options, no positionals (unknown
/// flags keep exit-1, VAL-202). `pub` because the bin crate's `main.rs`
/// dispatches `version` directly here.
pub fn cmd_version(args: &[String]) -> Result<String, CliError> {
    if let Some(a) = args.first() {
        return Err(CliError::usage(
            if a.starts_with('-') {
                format!("unknown option '{a}'")
            } else {
                format!("unexpected argument '{a}'")
            },
            a,
        ));
    }
    Ok(version_string())
}

/// Fold PR refs `1..counter` in id order for prose and JSON `pr list`.
fn fold_all<T>(
    store: &EventStore,
    head_ref: fn(u64) -> String,
    select: fn(&crate::event::FoldState) -> T,
) -> Result<Vec<T>, CliError> {
    let mut states = Vec::new();
    for n in 1..store.counter_next().map_err(CliError::from)? {
        let r = head_ref(n);
        if !crate::cli::store_has_ref(store, &r) {
            continue;
        }
        let chain = store.read_chain(&r).map_err(CliError::from)?;
        if chain.is_empty() {
            continue;
        }
        states.push(select(&fold(&chain)));
    }
    Ok(states)
}

/// The validated prose/JSON Issue display scan, in id order.
pub(crate) fn fold_all_issues(
    store: &EventStore,
) -> Result<Vec<crate::event::IssueState>, CliError> {
    let refs = store.read_issue_refs().map_err(CliError::from)?;
    for id in refs.tips.keys() {
        if !refs.created.contains(id)
            || !refs
                .states
                .get(id)
                .is_some_and(|state| state.id == *id && state.title.is_some())
        {
            return Err(CliError::invalid(format!(
                "cannot read {}: chain has no usable issue #{id} creation",
                crate::store::issue_ref(*id)
            )));
        }
    }
    Ok(refs.states.into_values().collect())
}

/// The prose/JSON `pr list` scan, in id order.
pub(crate) fn fold_all_prs(store: &EventStore) -> Result<Vec<crate::event::PrState>, CliError> {
    fold_all(store, crate::store::pr_head_ref, |s| s.pr.clone())
}

#[cfg(test)]
mod tests {
    use super::{issue_json, issue_list_json, json_mode, json_requested_with};

    #[test]
    fn json_split_keeps_option_values_and_end_of_options_as_data() {
        let split = |args: &[&str], values: &[&str]| {
            let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
            let (json, rest) = json_requested_with(&args, values);
            assert_eq!(json_mode(), Some(json));
            (json, rest)
        };
        let values = ["--label", "--"];
        assert_eq!(
            split(&["t", "--label", "--json"], &values),
            (false, vec!["t".into(), "--label".into(), "--json".into()])
        );
        assert_eq!(
            split(&["--json", "t", "--label", "--json", "--json"], &values),
            (true, vec!["t".into(), "--label".into(), "--json".into()])
        );
        assert_eq!(
            split(&["--label", "--", "--json"], &values),
            (true, vec!["--label".into(), "--".into()])
        );
        assert_eq!(
            split(&["--", "--json", "--label"], &values),
            (false, vec!["--".into(), "--json".into(), "--label".into()])
        );
        // Without declared values every `--json` is standalone; a trailing
        // value option keeps its missing value for the parser to reject.
        assert_eq!(
            split(&["--label", "--json", "--"], &[]),
            (true, vec!["--label".into(), "--".into()])
        );
        assert_eq!(
            split(&["--label"], &values),
            (false, vec!["--label".into()])
        );
    }
    use crate::event::IssueState;

    #[test]
    fn filtered_issue_json_uses_all_issues_for_blocked_and_children() {
        let mut all = vec![
            IssueState {
                id: 1,
                open: true,
                ..IssueState::default()
            },
            IssueState {
                id: 2,
                parent: Some(1),
                blocked_by: vec![3],
                open: true,
                ..IssueState::default()
            },
            IssueState {
                id: 3,
                open: true,
                ..IssueState::default()
            },
        ];
        let listed = issue_list_json(&[&all[1]], &all);
        assert!(listed.contains("\"blocked_by\":[3],\"blocked\":true"));
        assert!(!listed.contains("\"id\":3"));
        assert!(issue_json(&all[0], &all).contains("\"children\":[2]"));
        all[2].open = false;
        assert!(issue_list_json(&[&all[1]], &all).contains("\"blocked\":false"));
        assert!(issue_json(&all[1], &all).contains("\"blocked\":false"));
    }
}
