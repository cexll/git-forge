//! `--idempotency-key <k>` (#39) for `issue new`, `pr create`,
//! `issue comment` and `pr comment`: flag parsing, the canonical inputs that
//! define "the same request", and the replay of a recorded result. The record
//! storage and its atomic publication live in `crate::store`.
//!
//! A keyed command looks its record up with a read-only store BEFORE it
//! resolves its identity (the write barrier), so a replay writes nothing. A
//! first call publishes the record with its mutation in one transaction;
//! when a concurrent same-key call won, the store returns that record.
use crate::cli_error::CliError;
use crate::event::{Event, FoldState, JsonValue};
use crate::store::{
    Claim, Claimed, IdempotencyRecord, IdempotencyRequest, IdempotentCommand, RecordedResult,
    StoreError, MAX_KEY_BYTES,
};

/// The option name, shared by every keyed command parser.
pub(crate) const FLAG: &str = "--idempotency-key";

/// Validate the value that follows `--idempotency-key` and store it in
/// `slot`. Keys are opaque: any non-empty value up to [`MAX_KEY_BYTES`]
/// bytes, compared byte for byte.
pub(crate) fn set_key(slot: &mut Option<String>, value: Option<&String>) -> Result<(), CliError> {
    let value = value.ok_or_else(|| CliError::usage(format!("{FLAG} requires a value"), FLAG))?;
    if value.is_empty() {
        return Err(CliError::usage(
            format!("{FLAG} requires a non-empty value"),
            "",
        ));
    }
    if value.len() > MAX_KEY_BYTES {
        return Err(CliError::usage(
            format!("{FLAG} must be at most {MAX_KEY_BYTES} bytes"),
            value.as_str(),
        ));
    }
    if slot.replace(value.clone()).is_some() {
        return Err(CliError::usage(
            format!("{FLAG} may only be specified once"),
            FLAG,
        ));
    }
    Ok(())
}

/// Split `--idempotency-key <k>` out of a positional-only grammar (both
/// comment commands), returning the key and the remaining tokens.
pub(crate) fn take_key(args: &[String]) -> Result<(Option<String>, Vec<String>), CliError> {
    let mut key = None;
    let mut rest = Vec::with_capacity(args.len());
    let mut i = 0;
    while i < args.len() {
        if args[i] == FLAG {
            set_key(&mut key, args.get(i + 1))?;
            i += 2;
        } else {
            rest.push(args[i].clone());
            i += 1;
        }
    }
    Ok((key, rest))
}

/// The request for `key`, or `None` without a key. `fields` are the
/// command's normalized inputs in a fixed order; their canonical JSON
/// encoding is injective, and its git blob id is the recorded digest.
fn request(
    command: IdempotentCommand,
    key: Option<&str>,
    fields: &[(&str, JsonValue)],
) -> Result<Option<IdempotencyRequest>, CliError> {
    let Some(key) = key else {
        return Ok(None);
    };
    let escape = crate::event::json_escape_for_machine;
    let body: Vec<String> = fields
        .iter()
        .map(|(name, value)| format!("{}:{}", escape(name), crate::event::json_value(value)))
        .collect();
    let canonical = format!("{{{}}}", body.join(","));
    Ok(Some(IdempotencyRequest::new(command, key, &canonical)?))
}

fn opt_string(value: Option<&str>) -> JsonValue {
    value.map_or(JsonValue::Null, |v| JsonValue::String(v.to_string()))
}

fn opt_number(value: Option<u64>) -> JsonValue {
    value.map_or(JsonValue::Null, |v| JsonValue::Number(v as i64))
}

/// `issue new`: title, description, labels (in order), parent, and the
/// blocking issues (as a set: sorted).
pub(crate) fn issue_new_request(
    key: Option<&str>,
    title: &str,
    description: Option<&str>,
    labels: &[String],
    parent: Option<u64>,
    blocked_by: &[u64],
) -> Result<Option<IdempotencyRequest>, CliError> {
    let mut blocked_by = blocked_by.to_vec();
    blocked_by.sort_unstable();
    let blocked_by = blocked_by
        .into_iter()
        .map(|id| JsonValue::Number(id as i64));
    request(
        IdempotentCommand::IssueNew,
        key,
        &[
            ("title", JsonValue::String(title.to_string())),
            ("description", opt_string(description)),
            ("labels", crate::event::json_string_array(labels)),
            ("parent", opt_number(parent)),
            ("blocked_by", JsonValue::Array(blocked_by.collect())),
        ],
    )
}

/// `pr create`: title, the source and base branch NAMES as given (a
/// `refs/heads/` prefix removed; not their commits, so a retry after the
/// branch moved still replays), body, labels (in order), and closes.
pub(crate) fn pr_create_request(
    key: Option<&str>,
    title: &str,
    (source, base): (&str, &str),
    body: Option<&str>,
    labels: &[String],
    closes: Option<u64>,
) -> Result<Option<IdempotencyRequest>, CliError> {
    let branch = |name: &str| {
        JsonValue::String(name.strip_prefix("refs/heads/").unwrap_or(name).to_string())
    };
    request(
        IdempotentCommand::PrCreate,
        key,
        &[
            ("title", JsonValue::String(title.to_string())),
            ("source", branch(source)),
            ("base", branch(base)),
            ("body", opt_string(body)),
            ("labels", crate::event::json_string_array(labels)),
            ("closes", opt_number(closes)),
        ],
    )
}

/// `issue comment` / `pr comment`: the target id and the comment body.
pub(crate) fn comment_request(
    command: IdempotentCommand,
    key: Option<&str>,
    id: u64,
    body: &str,
) -> Result<Option<IdempotencyRequest>, CliError> {
    request(
        command,
        key,
        &[
            ("id", JsonValue::Number(id as i64)),
            ("body", JsonValue::String(body.to_string())),
        ],
    )
}

/// Before any write: the recorded output when this command kind and key
/// were already used (or `E_CONFLICT` for different inputs); `None` when the
/// key is new or absent.
pub(crate) fn replay(
    request: Option<&IdempotencyRequest>,
    json: bool,
) -> Result<Option<String>, CliError> {
    let Some(request) = request else {
        return Ok(None);
    };
    let store = crate::cli::open_store()?;
    match store.idempotency_record(request)? {
        Some(record) => resolve(request, record, json).map(Some),
        None => Ok(None),
    }
}

/// The outcome of a keyed command's write path. When it failed, a same-key
/// call may have committed after this call's first lookup and changed the
/// state its validation read (a deleted branch, Issue or PR): the committed
/// record answers before the failure does.
pub(crate) fn recheck(
    request: Option<&IdempotencyRequest>,
    json: bool,
    outcome: Result<String, CliError>,
) -> Result<String, CliError> {
    match outcome {
        Err(error) if request.is_some() => replay(request, json)?.ok_or(error),
        outcome => outcome,
    }
}

/// The output of a claimed mutation: this invocation's result when it
/// wrote, otherwise the earlier invocation's recorded result.
pub(crate) fn output<T>(
    request: Option<&IdempotencyRequest>,
    claimed: Claimed<T>,
    json: bool,
    written: impl FnOnce(T) -> Result<RecordedResult, StoreError>,
) -> Result<String, CliError> {
    match (claimed, request) {
        (Claimed::Existing(record), Some(request)) => resolve(request, record, json),
        (claimed, _) => Ok(pick(written(claimed.written()?)?, json)),
    }
}

fn pick(result: RecordedResult, json: bool) -> String {
    if json {
        result.json
    } else {
        result.text
    }
}

/// A record's output when its inputs match the request; `E_CONFLICT` naming
/// the recorded entity otherwise.
fn resolve(
    request: &IdempotencyRequest,
    record: IdempotencyRecord,
    json: bool,
) -> Result<String, CliError> {
    if record.inputs == request.inputs {
        return Ok(pick(record.result, json));
    }
    let entity = match request.command {
        IdempotentCommand::IssueNew | IdempotentCommand::IssueComment => "issue",
        IdempotentCommand::PrCreate | IdempotentCommand::PrComment => "PR",
    };
    let mut error = CliError::new(
        "E_CONFLICT",
        format!(
            "idempotency key '{}' was already used by {} with different inputs ({entity} #{})",
            crate::cli::sanitize_terminal(&request.key),
            request.command.name(),
            record.result.id
        ),
        request.key.as_str(),
        "Repeat the original inputs to read the recorded result, or use a new idempotency key.",
        false,
    );
    error.data = vec![
        ("command", JsonValue::String(request.command.name().into())),
        ("key", JsonValue::String(request.key.clone())),
        ("id", JsonValue::Number(record.result.id as i64)),
    ];
    Err(error)
}

/// Keyed comment, shared by `issue comment` and `pr comment`: resolve the
/// identity, check that entity `id` exists, then append the comment event
/// (built by `event` for the actor) and its record commit in one
/// transaction. `state` names the entity state folded at the appended
/// event. A failure re-checks the committed record ([`recheck`]).
pub(crate) fn comment(
    entity_ref: &str,
    id: u64,
    event: &dyn Fn(&str) -> Event,
    request: &IdempotencyRequest,
    json: bool,
    state: fn(&FoldState) -> &'static str,
) -> Result<String, CliError> {
    let entity = match request.command {
        IdempotentCommand::PrComment => "PR",
        _ => "issue",
    };
    let appended = (|| {
        let (store, actor) = crate::identity::open_mutation_store()?;
        if !crate::cli::store_has_ref(store.store(), entity_ref) {
            return Err(CliError::not_found(
                format!("{entity} #{id} does not exist"),
                id.to_string(),
            ));
        }
        let render = |id: u64, event: git2::Oid| {
            let folded = crate::event::fold(&store.read_chain_at(event)?);
            Ok(RecordedResult {
                id,
                json: crate::machine::event_result_json(id, state(&folded), &event.to_string()),
                text: format!("comment added to {entity} #{id}"),
            })
        };
        let claim = Claim {
            request,
            render: &render,
        };
        let claimed = store.append_event_claimed(entity_ref, &event(&actor), &claim)?;
        output(Some(request), claimed, json, |oid| render(id, oid))
    })();
    recheck(Some(request), json, appended)
}
