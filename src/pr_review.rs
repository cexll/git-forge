//! PR review argument handling and generation-bound event publication.
use std::collections::HashMap;

use crate::cli::{json_str, open_store, parse_entity_id, store_has_ref};
use crate::cli_error::CliError;
use crate::event::{Event, EventKind, JsonValue};
use crate::identity::open_mutation_store;

/// Value-taking options of `pr review`.
pub(crate) const REVIEW_VALUES: &[&str] = &["--file", "--line", "--commit"];

/// `git forge pr review <n> --approve|--reject [--file <f> --line <l> --commit <c>]`
pub(crate) fn cmd_pr_review(args: &[String]) -> Result<String, CliError> {
    const USAGE: &str =
        "usage: git forge pr review <n> --approve|--reject [--file <f> --line <l> --commit <c>]";
    let (json, args) = crate::machine::json_requested_with(args, REVIEW_VALUES);
    let args = args.as_slice();
    let id = parse_entity_id(args.first().ok_or_else(|| CliError::usage(USAGE, ""))?)?;
    let mut decision = None;
    let mut file = None;
    let mut line = None;
    let mut commit = None;
    let mut i = 1;
    let next_arg = |i: &mut usize, msg: &str| -> Result<String, CliError> {
        *i += 1;
        if *i >= args.len() {
            return Err(CliError::usage(msg, ""));
        }
        Ok(args[*i].clone())
    };
    while i < args.len() {
        match args[i].as_str() {
            "--approve" => decision = Some("approve".to_string()),
            "--reject" => decision = Some("reject".to_string()),
            "--file" => file = Some(next_arg(&mut i, "--file requires a path")?),
            "--line" => line = Some(next_arg(&mut i, "--line requires a number")?),
            "--commit" => commit = Some(next_arg(&mut i, "--commit requires a hash")?),
            h @ ("-h" | "--help") => return crate::cli::help_result(json, h, USAGE.into()),
            a if a.starts_with('-') => {
                return Err(CliError::usage(format!("unknown option '{a}'"), a))
            }
            _ => {}
        }
        i += 1;
    }
    let decision = decision.ok_or_else(|| CliError::usage(USAGE, ""))?;
    // Read-only path first: PR existence and terminality are validated WITHOUT
    // resolving the mutable identity, so every refusal resolves nothing and
    // writes nothing. The append below reopens the store bound to the actor.
    let store = open_store()?;
    let r = crate::store::pr_head_ref(id);
    if !store_has_ref(&store, &r) {
        return Err(CliError::not_found(
            format!("PR #{id} does not exist"),
            id.to_string(),
        ));
    }
    let chain = store.read_chain(&r).map_err(CliError::from)?;
    let state = crate::event::fold(&chain).pr;
    // Abandoned PRs are terminal (pr.abandoned): refuse BEFORE any write and
    // name the abandon reason, so an abandoned PR never gains a review event.
    if let Some(reason) = &state.abandoned {
        return Err(CliError::invalid(format!(
            "PR #{id} is abandoned: {reason}"
        )));
    }
    let (store, actor) = open_mutation_store()?;
    // FR-005: inline review comments anchor to a commit hash. --file/--line
    // are only meaningful as inline comments, so they require the anchor;
    // an unanchored inline review would not reference anything immutable.
    if (file.is_some() || line.is_some()) && commit.is_none() {
        return Err(CliError::usage("inline review requires --commit <hash> to anchor the comment (FR-005); add --commit or drop --file/--line", ""));
    }
    // The anchor must resolve to a real commit object. It is intentionally NOT
    // constrained to the PR snapshot: spec.md explicitly permits inline
    // comments on commits outside the PR snapshot (anchored reference). The
    // resolved commit OID is what gets stored — never the raw input — so a
    // mutable ref like `main` cannot smuggle a moving anchor into the event.
    let commit = match commit {
        Some(c) => Some(
            store
                .repo()
                .revparse_single(&c)
                .and_then(|o| o.peel_to_commit())
                .map(|c| c.id().to_string())
                .map_err(|_| {
                    format!(
                        "inline review --commit '{c}' does not resolve to a commit \
                         (FR-005 anchor must be a real commit)"
                    )
                })?,
        ),
        None => None,
    };
    let mut body = HashMap::new();
    body.insert("decision".into(), json_str(&decision));
    if let Some(f) = file {
        body.insert("file".into(), json_str(&f));
    }
    if let Some(l) = line {
        body.insert("line".into(), json_str(&l));
    }
    if let Some(c) = commit {
        // Inline comment anchored to a commit (immutable; never follows later
        // diff changes).
        body.insert("commit".into(), json_str(&c));
    }
    // Generation binding (F-006): the decision authorizes the snapshot
    // generation CAPTURED at review time — the fold state read at command
    // start, before the append. `append_event`'s CAS retry re-parents the
    // SAME event body onto a moved tip, so an in-flight gen0 review whose
    // append lands after a concurrent `pr update` still carries generation 0;
    // the merge gate refuses it as stale instead of folding it as gen1.
    // (The gate's decision check compares this captured generation with the
    // PR's CURRENT generation at merge time.)
    body.insert(
        "generation".into(),
        JsonValue::Number(state.generation as i64),
    );
    // #2 result-bound review: the decision also records the LIVE base-branch
    // tip at review time, so the live-base merge gate can require a decision
    // bound to the current base tip when the base has moved since the PR
    // snapshot.
    if let Some(bh) =
        crate::pr_prepare::live_base_head_entry(store.repo(), state.base_ref.as_deref())
    {
        body.insert("base_head".into(), bh);
    }
    // F-002 candidate-bound review: when a prepared candidate exists at
    // review time (fold candidate whose generation matches the current
    // snapshot AND whose identity is corroborated by the pinned result ref),
    // the decision binds THAT candidate (strategy/result_commit/result_tree
    // stamped from the CAPTURED fold state, not re-read after the append).
    // The merge gate requires decision identity == candidate identity, so a
    // stale approve (bound to a superseded candidate) or an unbound legacy
    // approve refuses with a re-review remedy.
    let candidate_bind = match &state.prepared {
        Some(p) if p.generation == state.generation => store
            .repo()
            .find_reference(&crate::store::pr_result_ref(id))
            .ok()
            .and_then(|r| r.target())
            .filter(|pin| pin.to_string() == p.result_commit)
            .map(|_| p.clone()),
        _ => None,
    };
    if let Some(c) = &candidate_bind {
        body.insert("strategy".into(), json_str(&c.strategy));
        body.insert("result_commit".into(), json_str(&c.result_commit));
        body.insert("result_tree".into(), json_str(&c.result_tree));
    }
    let ev = Event::new(EventKind::PrReview, "pr", id, &actor, body);
    let event = store.append_event(&r, &ev).map_err(CliError::from)?;
    if json {
        return Ok(crate::machine::pr_review_json(
            id,
            crate::machine::pr_state_at(store.store(), event)?,
            &decision,
            state.generation,
            candidate_bind.as_ref().map(|c| c.result_commit.as_str()),
            &event.to_string(),
        ));
    }
    Ok(format!("reviewed PR #{id} ({decision})"))
}
