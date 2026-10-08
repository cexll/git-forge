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

/// Strip bare `--json` flag tokens from `args`, returning whether at least
/// one was present plus the remaining arguments. `--json` is a pure
/// output-mode switch, so — unlike value-taking options — it is recognized in
/// ANY position (before or after positionals); `--json=x` is NOT matched.
/// This seam ONLY owns the machine-flag split: whether the leftovers are
/// grammatical belongs to each per-command parser, which rejects any token it
/// does not know — including a leftover `--json=x` — with a clean
/// `unknown option '<flag>'` usage error before any side effect (exit-1 is
/// unchanged, VAL-202). Commands with NO options at all call
/// [`reject_leftover_flags`] on the remainder; option-taking parsers reject
/// in their own match arms, where `--source` etc. are known.
pub(crate) fn json_requested(args: &[String]) -> (bool, Vec<String>) {
    let mut json = false;
    let mut rest = Vec::with_capacity(args.len());
    for a in args {
        if a == "--json" {
            json = true;
        } else {
            rest.push(a.clone());
        }
    }
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

/// `ci` member of the pr schemas: `{"status":..,"plan":..}` or `null`.
fn ci_json(status: Option<&str>, plan: Option<&str>) -> String {
    match status {
        Some(s) => format!(
            "{{\"status\":{},\"plan\":{}}}",
            json_escape(s),
            opt_str(plan),
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
        ci_json(state.ci_status.as_deref(), state.ci_plan.as_deref()),
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

/// Fold every existing entity ref `1..counter` in id order — the shared scan
/// behind prose and JSON `issue list` / `pr list`, so the two surfaces can
/// never disagree on WHICH entities exist — only on rendering. One generic
/// body (not a per-entity pair): the duplication gate counts the naive pair.
fn fold_all<T>(
    store: &EventStore,
    head_ref: fn(u64) -> String,
    select: fn(&crate::event::FoldState) -> T,
) -> Result<Vec<T>, CliError> {
    let mut states = Vec::new();
    for n in 1..store.counter_next().ok().unwrap_or(1) {
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

/// The prose/JSON `issue list` scan, in id order.
pub(crate) fn fold_all_issues(
    store: &EventStore,
) -> Result<Vec<crate::event::IssueState>, CliError> {
    fold_all(store, crate::store::issue_ref, |s| s.issue.clone())
}

/// The prose/JSON `pr list` scan, in id order.
pub(crate) fn fold_all_prs(store: &EventStore) -> Result<Vec<crate::event::PrState>, CliError> {
    fold_all(store, crate::store::pr_head_ref, |s| s.pr.clone())
}

#[cfg(test)]
mod tests {
    use super::{issue_json, issue_list_json};
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
