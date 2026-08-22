//! CLI identity resolution: map the repo's configured identity to a forge
//! committer signature and event actor, via the safe git binary.
//!
//! Every forge write needs an explicit identity. libgit2 config reads after
//! `Repository::open` can SIGSEGV (VAL-115 STAGE A / libgit2 1.9.6) when the
//! file is corrupted, so identity is resolved here through
//! [`crate::git::config_get_identity`] (a `git config --null --get` child
//! process) — never through libgit2.

use crate::store::{BoundEventStore, EventStore};

/// Resolve the repo identity from the git binary (crate::git::config_get_identity
/// — never libgit2 config, which would SIGSEGV on a config corrupted after
/// `Repository::open`). Returns the committer signature and the event actor.
/// STAGE A: a config-open parse failure propagates as Err — no silent
/// fallback, no empty actor. A malformed .git/config surfaces here as a clean
/// CLI error (git config --get exits 128).
pub(crate) fn resolve_identity() -> Result<(git2::Signature<'static>, String), String> {
    let (name, email) = crate::git::config_get_identity(std::path::Path::new("."))?;
    // Commit signature: configured name+email ONLY when both are usable AND
    // form a valid sig; any failure falls back to the default committer —
    // legacy `repo.signature()` behavior. The ACTOR keeps the resolved
    // non-empty email regardless (F-028: actor is email-only).
    let signature = match (&name, &email) {
        (Some(n), Some(e)) => match git2::Signature::now(n, e) {
            Ok(s) => s,
            Err(_) => {
                git2::Signature::now("git-forge", "forge@localhost").map_err(|e| e.to_string())?
            }
        },
        _ => git2::Signature::now("git-forge", "forge@localhost").map_err(|e| e.to_string())?,
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
pub(crate) fn open_mutation_store() -> Result<(BoundEventStore, String), String> {
    let (signature, actor) = resolve_identity()?;
    let store = EventStore::open(".").map_err(|e| format!("{e}"))?;
    Ok(bind_identity(store, signature, actor))
}
