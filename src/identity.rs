//! CLI identity resolution: map the repo's configured identity to a forge
//! committer signature and event actor, via the safe git binary.
//!
//! Every forge write needs an explicit identity. libgit2 config reads after
//! `Repository::open` can SIGSEGV (VAL-115 STAGE A / libgit2 1.9.6) when the
//! file is corrupted, so identity is resolved here through
//! [`crate::git::config_get_identity`] (a `git config --null --get` child
//! process) — never through libgit2.
use crate::cli_error::CliError;

use crate::store::{BoundEventStore, EventStore};

/// The command's resolved role, if any. `None` = no role was configured
/// (the single-user default: events carry no `role` key, the wire shape is
/// byte-identical to pre-t8). Set exactly once by [`resolve_role`]; read at
/// write time by [`stamped_event_json`].
static COMMAND_ROLE: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();

/// The three first-class roles an event actor can carry (#8). The set is
/// closed fail-closed: any other `GIT_FORGE_ROLE` value is a clean CLI error
/// BEFORE any write, never silently recorded verbatim.
pub(crate) const ROLES: [&str; 3] = ["worker", "reviewer", "orchestrator"];

/// Resolve the command's role from `GIT_FORGE_ROLE` (absent, empty, or
/// whitespace = no role). An unknown value errors — fail-closed, before any
/// store open or write, mirroring F-028's no-silent-fallback policy.
/// OnceLock: the first resolution wins for the process; a mutation command
/// resolves identity at most once, so this is single-shot in practice.
fn resolve_role() -> Result<Option<String>, CliError> {
    let raw = std::env::var("GIT_FORGE_ROLE").unwrap_or_default();
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok(COMMAND_ROLE.get_or_init(|| None).clone());
    }
    if !ROLES.contains(&raw) {
        return Err(CliError::new(
            "E_INVALID",
            format!(
                "unknown GIT_FORGE_ROLE '{raw}' (expected one of: {})",
                ROLES.join(", ")
            ),
            raw,
            "Correct the input or repository state before retrying.",
            false,
        ));
    }
    let role = Some(raw.to_string());
    Ok(COMMAND_ROLE.get_or_init(|| role.clone()).clone())
}

/// The command's resolved role, if any (read side; `None` before the first
/// identity resolution of this process — read-only commands never resolve).
pub(crate) fn command_role() -> Option<String> {
    COMMAND_ROLE.get().cloned().flatten()
}

/// Serialize `event` for the wire, stamping the command's resolved role as
/// an additive `role` body field when one is configured. THE write-time
/// serialization path: `write_event_commit` calls this instead of
/// `Event::to_json`, so every event a role-attributed command writes carries
/// the same role regardless of which code path built the event. No role →
/// `to_json()` verbatim (the pre-t8 wire shape, byte-for-byte).
pub(crate) fn stamped_event_json(event: &crate::event::Event) -> String {
    match command_role() {
        Some(role) => {
            let mut body = event.body.clone();
            body.insert("role".into(), crate::event::JsonValue::String(role));
            let stamped = crate::event::Event {
                body,
                ..event.clone()
            };
            stamped.to_json()
        }
        None => event.to_json(),
    }
}

/// Resolve the repo identity from the git binary (crate::git::config_get_identity
/// — never libgit2 config, which would SIGSEGV on a config corrupted after
/// `Repository::open`). Returns the committer signature and the event actor.
/// STAGE A: a config-open parse failure propagates as Err — no silent
/// fallback, no empty actor. A malformed .git/config surfaces here as a clean
/// CLI error (git config --get exits 128).
pub(crate) fn resolve_identity() -> Result<(git2::Signature<'static>, String), CliError> {
    let (name, email) = crate::git::config_get_identity(std::path::Path::new("."))?;
    // Commit signature: configured name+email ONLY when both are usable AND
    // form a valid sig; any failure falls back to the default committer —
    // legacy `repo.signature()` behavior. The ACTOR keeps the resolved
    // non-empty email regardless (F-028: actor is email-only).
    // Role resolved FIRST (fail-closed before any config read or store
    // open): an invalid GIT_FORGE_ROLE is a clean CLI error that resolves
    // nothing and writes nothing.
    resolve_role()?;
    let signature = match (&name, &email) {
        (Some(n), Some(e)) => match git2::Signature::now(n, e) {
            Ok(s) => s,
            Err(_) => git2::Signature::now("git-forge", "forge@localhost")
                .map_err(|e| CliError::from(e.to_string()))?,
        },
        _ => git2::Signature::now("git-forge", "forge@localhost")
            .map_err(|e| CliError::from(e.to_string()))?,
    };
    let actor = email.unwrap_or_else(|| "forge@localhost".to_string());
    Ok((signature, actor))
}

/// Bind an already-open store's identity (precomputed by `resolve_identity`)
/// and return it with the actor. Call immediately before any forge commit/ref
/// write (after read-only validation), so a config fault surfaces before any
/// mutation and never leaks a partially-created ref.
pub(crate) fn bind_identity(
    store: EventStore,
    signature: git2::Signature<'static>,
    actor: String,
) -> (BoundEventStore, String) {
    (store.bind_signature(signature), actor)
}

/// Open + bind for a MUTATION command. Identity is resolved BEFORE the
/// libgit2 store is opened, so a malformed .git/config fails here as a clean
/// CLI error, never as an libgit2 open failure or post-open SIGSEGV.
pub(crate) fn open_mutation_store() -> Result<(BoundEventStore, String), CliError> {
    let (signature, actor) = resolve_identity()?;
    let store = EventStore::open(".").map_err(CliError::from)?;
    Ok(bind_identity(store, signature, actor))
}
