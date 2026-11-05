use crate::EventsError;

/// The code of the warning the events consumer itself returns when an event note could not be written.
pub const WRITE_FAILED_CODE: &str = "events/write-failed";

/// Whether a warning means a secondary service failed or is only informing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WarningClass {
    /// A secondary service could not do its job: the vault is degraded until it can.
    Failure,
    /// Routine information, or a code a write never raises, that must not turn the vault amber.
    Notice,
}

use WarningClass::{Failure, Notice};

/// Every warning code the workspace can produce, classified once.
///
/// The codes come from the error types of the Git, index, search, operation-log, and projection services and from the
/// server's own scheduling and locking, so the list is kept by the workspace scan in this module's tests: a code
/// added anywhere without an entry here fails them. A code that only a query raises, or that reports a service the
/// operator turned off, is a notice because it is never evidence that a write's follow-up work failed.
pub(crate) const CLASSIFIED: &[(&str, WarningClass)] = &[
    ("embedding/config", Failure),
    ("embedding/local-unavailable", Failure),
    ("embedding/network", Failure),
    ("embedding/provider-error", Failure),
    ("embedding/response-too-large", Failure),
    ("embedding/timeout", Failure),
    ("events/write-failed", Failure),
    ("git/commit", Failure),
    ("git/disabled", Notice),
    ("git/init", Failure),
    ("git/inspect", Failure),
    ("git/not-a-repo", Failure),
    ("git/restore", Failure),
    ("git/schedule", Failure),
    ("git/stage", Failure),
    ("git/write", Failure),
    ("index/busy", Failure),
    ("index/config", Failure),
    ("index/corrupt", Failure),
    ("index/disabled", Notice),
    ("index/entities", Failure),
    ("index/in-use", Failure),
    ("index/invalid-query", Notice),
    ("index/legacy-conflict", Failure),
    ("index/locked", Failure),
    ("index/reconcile", Failure),
    ("index/root", Failure),
    ("index/stale", Failure),
    ("index/storage", Failure),
    ("index/undecodable", Failure),
    ("log/append", Failure),
    ("log/disabled", Notice),
    ("projection/busy", Failure),
    ("projection/cleanup-incomplete", Failure),
    ("projection/configuration-unnamed", Failure),
    ("projection/foreign-owner", Failure),
    ("projection/hand-edited-replaced", Notice),
    ("projection/home-unavailable", Failure),
    ("projection/invalid-configuration", Failure),
    ("projection/invalid-definition", Failure),
    ("projection/invalid-slug", Failure),
    ("projection/linked-folder", Failure),
    ("projection/lock-failed", Failure),
    ("projection/malformed-block", Failure),
    ("projection/manifest-malformed", Failure),
    ("projection/path-rejected", Failure),
    ("projection/read-failed", Failure),
    ("projection/reparse-point", Failure),
    ("projection/state-root-in-target", Failure),
    ("projection/target-in-state-root", Failure),
    ("projection/target-in-vault", Failure),
    ("projection/target-not-absolute", Failure),
    ("projection/target-unresolvable", Failure),
    ("projection/targets-overlap", Failure),
    ("projection/unowned-collision", Failure),
    ("projection/vault-in-target", Failure),
    ("projection/write-failed", Failure),
    ("search/failed", Notice),
    ("search/malformed-response", Notice),
    ("vector/config", Failure),
    ("vector/storage", Failure),
    ("write/line-endings-converted", Notice),
];

impl TryFrom<&str> for WarningClass {
    type Error = EventsError;

    fn try_from(code: &str) -> Result<Self, EventsError> {
        CLASSIFIED
            .iter()
            .find(|(known, _)| *known == code)
            .map(|(_, class)| *class)
            .ok_or_else(|| EventsError::UnclassifiedWarning { code: code.to_owned() })
    }
}

/// The service a warning code reports on: the part before the slash, or the whole code when it has none.
#[must_use]
pub fn service_name(code: &str) -> &str {
    code.split_once('/').map_or(code, |(service, _)| service)
}

#[cfg(test)]
#[path = "warning_codes_test.rs"]
mod tests;
