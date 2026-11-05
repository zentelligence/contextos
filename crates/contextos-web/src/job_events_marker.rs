//! The host's note of when a vault's job events were first reconciled, which sets how far back the first pass looks.

use std::path::PathBuf;

use time::format_description::well_known::Rfc3339;
use time::{OffsetDateTime, UtcOffset};

use crate::job_attention::MarkerProblem;
use crate::run_settings::HostContext;

/// The directory, under the host's claims directory, that holds the notes. A workflow's claims live under
/// `vaults/<vault>/<workflow>/`, so a note kept here can never share a name with a workflow's directory.
const MARKER_DIR: &str = ".job-events-reconciled";

/// What the host knows of a vault's first reconcile.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Reconciled {
    /// The host has not reconciled the vault, or cannot say.
    Never,
    /// The host first reconciled the vault at this time.
    Since(OffsetDateTime),
    /// The host has a note that cannot be read or is not a time.
    Unusable,
}

impl Reconciled {
    #[must_use]
    pub const fn since(self) -> Option<OffsetDateTime> {
        match self {
            Self::Since(at) => Some(at),
            Self::Never | Self::Unusable => None,
        }
    }

    #[must_use]
    pub const fn is_never(self) -> bool {
        matches!(self, Self::Never)
    }
}

/// Where the note for `vault_name` lives, or `None` when the host has no claims directory or the name could leave it.
#[must_use]
pub fn marker_path(host: &HostContext, vault_name: &str) -> Option<PathBuf> {
    let claims_root = host.claims_root.as_ref()?;
    crate::run_claim::is_plain_segment(vault_name)
        .then(|| claims_root.join(MARKER_DIR).join(vault_name.to_ascii_lowercase()))
}

/// What the host knows of when it first reconciled `vault_name`.
///
/// A missing note means the vault has not been reconciled. A note that cannot be read, or is not a time, is
/// [`Reconciled::Unusable`] and reported once until it reads again: the caller keeps the floor at the lookback from
/// now, the earliest instant that cannot record an old failure as new, and leaves the note alone for the operator to
/// see rather than overwrite it with a later time.
pub async fn read(host: &HostContext, vault_name: &str) -> Reconciled {
    let Some(path) = marker_path(host, vault_name) else {
        return Reconciled::Never;
    };
    let text = tokio::task::spawn_blocking(move || std::fs::read_to_string(path)).await;
    let memory = &host.attention;
    let (outcome, problem) = match text {
        Ok(Ok(text)) => match OffsetDateTime::parse(text.trim(), &Rfc3339) {
            Ok(at) => (Reconciled::Since(at), None),
            Err(_) => (Reconciled::Unusable, Some(MarkerProblem::Corrupt)),
        },
        Ok(Err(error)) if error.kind() == std::io::ErrorKind::NotFound => (Reconciled::Never, None),
        Ok(Err(_)) | Err(_) => (Reconciled::Unusable, Some(MarkerProblem::Unreadable)),
    };
    for candidate in [MarkerProblem::Corrupt, MarkerProblem::Unreadable] {
        let holds = problem == Some(candidate);
        if memory.note_marker_problem(candidate, vault_name, holds) {
            tracing::warn!(
                vault = vault_name,
                problem = ?candidate,
                "the host's note of when the vault's job events were first reconciled cannot be used, so only the last \
                 seven days of runs are considered until it is removed or repaired"
            );
        }
    }
    outcome
}

/// Records that this host has reconciled `vault_name` for events, so later passes consider every run they read. A
/// failure is reported once until a write succeeds.
pub async fn write(host: &HostContext, vault_name: &str, now: OffsetDateTime) {
    let Some(path) = marker_path(host, vault_name) else {
        return;
    };
    let Ok(stamp) = now.to_offset(UtcOffset::UTC).format(&Rfc3339) else {
        return;
    };
    let written = tokio::task::spawn_blocking(move || {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        crate::atomic_write::write_atomically(&path, format!("{stamp}\n").as_bytes())
    })
    .await;
    let failed = !matches!(written, Ok(Ok(())));
    if host
        .attention
        .note_marker_problem(MarkerProblem::Unwritable, vault_name, failed)
    {
        tracing::warn!(
            vault = vault_name,
            "the host could not note that the vault's job events were reconciled"
        );
    }
}

#[cfg(test)]
#[path = "job_events_marker_test.rs"]
mod tests;
