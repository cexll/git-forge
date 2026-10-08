//! Idempotency records (#39): one immutable ref per command kind and key,
//! published in the SAME `update-ref` transaction as the mutation it records.
//!
//! Ref: `refs/forge/idempotency/<command>/<hex>`, where `<hex>` is the
//! lowercase hex of the key's bytes (injective and always a legal ref
//! component). Target: a root commit whose tree holds one blob,
//! `.forge/idempotency.json`:
//! `{"v":1,"command":..,"key":..,"inputs":<digest>,"id":..,"json":..,"text":..}`.
//! `inputs` is the git blob id of the command's canonical inputs; `json` and
//! `text` are the exact `--json` and human results of the recorded mutation.
//! The record ref is created with expected absence, so of two concurrent
//! claims exactly one transaction commits; the loser reads the winner's record.
use super::*;

/// Longest accepted key in bytes: its hex form stays well under the 255-byte
/// file-name limit of the files ref backend.
pub(crate) const MAX_KEY_BYTES: usize = 100;

/// Record blob path inside the record commit's tree.
const RECORD_PATH: &str = ".forge/idempotency.json";

/// A command kind with its own key namespace.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum IdempotentCommand {
    IssueNew,
    PrCreate,
    IssueComment,
    PrComment,
}

impl IdempotentCommand {
    /// Ref path component of the command's key namespace.
    pub(crate) fn slug(self) -> &'static str {
        match self {
            Self::IssueNew => "issue-new",
            Self::PrCreate => "pr-create",
            Self::IssueComment => "issue-comment",
            Self::PrComment => "pr-comment",
        }
    }
    /// The command as the user types it.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::IssueNew => "issue new",
            Self::PrCreate => "pr create",
            Self::IssueComment => "issue comment",
            Self::PrComment => "pr comment",
        }
    }
}

/// One keyed invocation: the command kind, the key, and the digest of the
/// canonical inputs that define "the same request".
pub(crate) struct IdempotencyRequest {
    pub command: IdempotentCommand,
    pub key: String,
    pub inputs: Oid,
}

impl IdempotencyRequest {
    pub(crate) fn new(
        command: IdempotentCommand,
        key: &str,
        canonical_inputs: &str,
    ) -> Result<Self, StoreError> {
        let inputs = Oid::hash_object(git2::ObjectType::Blob, canonical_inputs.as_bytes())?;
        Ok(Self {
            command,
            key: key.to_string(),
            inputs,
        })
    }
    /// The record ref for this command kind and key.
    pub(crate) fn ref_name(&self) -> String {
        let hex: String = self.key.bytes().map(|b| format!("{b:02x}")).collect();
        format!("refs/forge/idempotency/{}/{hex}", self.command.slug())
    }
}

/// The result of a recorded mutation, as both output modes print it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RecordedResult {
    pub id: u64,
    pub json: String,
    pub text: String,
}

/// A stored record, read back and validated against its request.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct IdempotencyRecord {
    pub inputs: Oid,
    pub result: RecordedResult,
}

/// The outcome of a claimed mutation: this transaction committed, or an
/// earlier invocation with the same key already holds the record.
#[derive(Debug)]
pub(crate) enum Claimed<T> {
    Written(T),
    Existing(IdempotencyRecord),
}

impl<T> Claimed<T> {
    /// The written value of an unclaimed mutation, which never meets a record.
    pub(crate) fn written(self) -> Result<T, StoreError> {
        match self {
            Claimed::Written(value) => Ok(value),
            Claimed::Existing(_) => Err(StoreError::InvalidState(
                "unkeyed mutation met an idempotency record".into(),
            )),
        }
    }
}

/// Renders the result of entity `id` whose mutation is event commit `event`;
/// called once per transaction attempt, before the transaction runs.
pub(crate) type Render<'a> = &'a dyn Fn(u64, Oid) -> Result<RecordedResult, StoreError>;

/// The record a mutation publishes with its own refs.
pub(crate) struct Claim<'a> {
    pub request: &'a IdempotencyRequest,
    pub render: Render<'a>,
}

fn record_blob(request: &IdempotencyRequest, result: &RecordedResult) -> String {
    let escape = crate::event::json_escape_for_machine;
    format!(
        "{{\"v\":1,\"command\":{},\"key\":{},\"inputs\":{},\"id\":{},\"json\":{},\"text\":{}}}",
        escape(request.command.name()),
        escape(&request.key),
        escape(&request.inputs.to_string()),
        result.id,
        escape(&result.json),
        escape(&result.text),
    )
}

/// Parse and validate a record blob for `request` (persisted state is a
/// trust boundary: every field must be present, typed, and match the ref).
fn parse_record(request: &IdempotencyRequest, content: &str) -> Option<IdempotencyRecord> {
    let JsonValue::Object(map) = crate::event::parse_json_value(content)? else {
        return None;
    };
    let text_field = |name: &str| map.get(name).and_then(JsonValue::as_str);
    let id = match map.get("id")? {
        JsonValue::Number(n) if *n > 0 => *n as u64,
        _ => return None,
    };
    let valid = map.len() == 7
        && matches!(map.get("v"), Some(JsonValue::Number(1)))
        && text_field("command") == Some(request.command.name())
        && text_field("key") == Some(request.key.as_str());
    if !valid {
        return None;
    }
    Some(IdempotencyRecord {
        inputs: Oid::from_str(text_field("inputs")?).ok()?,
        result: RecordedResult {
            id,
            json: text_field("json")?.to_string(),
            text: text_field("text")?.to_string(),
        },
    })
}

impl EventStore {
    /// The record for `request`'s command kind and key, if one exists. A
    /// record ref that is not a well-formed record for this key is an error.
    pub(crate) fn idempotency_record(
        &self,
        request: &IdempotencyRequest,
    ) -> Result<Option<IdempotencyRecord>, StoreError> {
        let name = request.ref_name();
        let Some(tip) = self.current_tip(&name)? else {
            return Ok(None);
        };
        let malformed = || StoreError::InvalidState(format!("malformed idempotency record {name}"));
        let commit = self.repo.find_commit(tip).map_err(|_| malformed())?;
        let entry = commit
            .tree()?
            .get_path(Path::new(RECORD_PATH))
            .map_err(|_| malformed())?;
        let object = entry.to_object(&self.repo)?;
        let content = object
            .as_blob()
            .and_then(|blob| std::str::from_utf8(blob.content()).ok())
            .ok_or_else(malformed)?;
        parse_record(request, content)
            .map(Some)
            .ok_or_else(malformed)
    }
}

impl BoundEventStore {
    /// Write the record commit for `claim` and return the transaction line
    /// that creates its ref with expected absence.
    pub(crate) fn claim_line(
        &self,
        claim: &Claim<'_>,
        id: u64,
        event: Oid,
    ) -> Result<String, StoreError> {
        let result = (claim.render)(id, event)?;
        let blob = record_blob(claim.request, &result);
        let message = format!("forge:idempotency:{}", claim.request.command.slug());
        let leaf = RECORD_PATH.trim_start_matches(".forge/");
        let commit = self.write_forge_commit(leaf, blob.as_bytes(), &[], &message)?;
        Ok(format!(
            "update {} {commit} {ZERO_OID}",
            claim.request.ref_name()
        ))
    }

    /// After a failed claimed transaction: the record that an earlier
    /// invocation published, which explains the failure.
    pub(crate) fn existing_claim(
        &self,
        claim: Option<&Claim<'_>>,
    ) -> Result<Option<IdempotencyRecord>, StoreError> {
        match claim {
            Some(claim) => self.inner.idempotency_record(claim.request),
            None => Ok(None),
        }
    }

    /// [`Self::append_event`] that publishes `claim`'s record in the same
    /// transaction as the appended event. On a CAS conflict without a record
    /// the SAME event is re-parented onto the fresh tip (bounded retries).
    pub(crate) fn append_event_claimed(
        &self,
        entity_ref: &str,
        event: &Event,
        claim: &Claim<'_>,
    ) -> Result<Claimed<Oid>, StoreError> {
        let message = format!("forge:{}:{}", event.kind.as_str(), event.entity_id);
        let mut tip = self.inner.current_tip(entity_ref)?;
        for _ in 0..MAX_APPEND_RETRIES {
            let parents: Vec<Oid> = tip.into_iter().collect();
            let new_oid = self.write_event_commit(event, &parents, &message)?;
            let old = tip.map_or_else(|| ZERO_OID.to_string(), |o| o.to_string());
            let lines = [
                format!("update {entity_ref} {new_oid} {old}"),
                self.claim_line(claim, event.entity_id, new_oid)?,
            ];
            match self.inner.run_update_ref_stdin(&lines) {
                Ok(()) => return Ok(Claimed::Written(new_oid)),
                Err(error) => {
                    if let Some(record) = self.existing_claim(Some(claim))? {
                        return Ok(Claimed::Existing(record));
                    }
                    let fresh = self.inner.current_tip(entity_ref)?;
                    if fresh == tip {
                        return Err(error);
                    }
                    tip = fresh;
                }
            }
        }
        Err(StoreError::Exhausted)
    }
}

#[cfg(test)]
mod tests;
