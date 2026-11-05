use std::collections::BTreeSet;
use std::path::Path;

use contextos_core::{OperationService, OperationWarning};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::persist::{read_json, write_json};
use crate::{
    EventRecord, EventsError, FieldName, FieldValue, KindName, ServerKind, WarningClass, resolve_entity, service_name,
};

/// A change in the vault's health that one write brought about.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HealthChange {
    /// A failure code appeared that was not current.
    Degraded { code: String, service: String },
    /// A current failure code's service ran cleanly.
    Recovered { code: String, service: String },
}

/// The failure codes a vault is currently degraded by: derived state kept so each failure is reported once and its
/// clearing is reported once.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct CurrentFailures {
    codes: BTreeSet<String>,
}

/// Reports whether the service behind `code` took part in the write: one the batch was delivered to, or the events
/// service, which runs for every tool write. A code of a service this build does not know about counts as applied, so
/// it can never stay current for ever.
fn applied(code: &str, attempted: &[OperationService]) -> bool {
    let service = match service_name(code) {
        "git" => OperationService::Git,
        "index" => OperationService::Index,
        "search" | "embedding" | "vector" => OperationService::Search,
        "projection" => OperationService::Projection,
        "log" => OperationService::OperationLog,
        _ => return true,
    };
    attempted.contains(&service)
}

impl CurrentFailures {
    /// Whether `code` is a current failure.
    #[must_use]
    pub fn contains(&self, code: &str) -> bool {
        self.codes.contains(code)
    }

    /// Whether no failure is current.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.codes.is_empty()
    }

    /// Compares one completed write's warnings with the current failures and returns what changed.
    ///
    /// Newly failing codes come first, in the order the warnings gave them, then the codes that cleared, in code
    /// order. A code clears only when its service took part in the write (`attempted`) and raised no warning with
    /// that code. A code nobody classified counts as a failure, so a new warning is never hidden.
    pub fn observe(&mut self, warnings: &[OperationWarning], attempted: &[OperationService]) -> Vec<HealthChange> {
        let mut failing: Vec<&str> = Vec::new();
        for warning in warnings {
            let class = WarningClass::try_from(warning.code.as_str()).unwrap_or_else(|_| {
                tracing::warn!(code = %warning.code, "warning code has no failure or notice classification");
                WarningClass::Failure
            });
            if class == WarningClass::Failure && !failing.contains(&warning.code.as_str()) {
                failing.push(&warning.code);
            }
        }
        let mut changes = Vec::new();
        for code in &failing {
            if self.codes.insert((*code).to_owned()) {
                changes.push(HealthChange::Degraded {
                    code: (*code).to_owned(),
                    service: service_name(code).to_owned(),
                });
            }
        }
        let cleared: Vec<String> = self
            .codes
            .iter()
            .filter(|code| !failing.contains(&code.as_str()) && applied(code, attempted))
            .cloned()
            .collect();
        for code in cleared {
            self.codes.remove(&code);
            changes.push(HealthChange::Recovered {
                service: service_name(&code).to_owned(),
                code,
            });
        }
        changes
    }

    /// Reads the failures from `path`; `None` when the file does not exist.
    ///
    /// # Errors
    ///
    /// Returns [`EventsError::TableUnreadable`] when the file cannot be read; the caller starts from none.
    pub fn load(path: &Path) -> Result<Option<Self>, EventsError> {
        read_json(path)
    }

    /// Writes the failures to `path` atomically, creating its folder when missing.
    ///
    /// # Errors
    ///
    /// Returns [`EventsError::TableUnwritable`] when the file cannot be written.
    pub fn store(&self, path: &Path) -> Result<(), EventsError> {
        write_json(path, self)
    }
}

impl From<(&HealthChange, OffsetDateTime)> for EventRecord {
    /// The event for a health change at `at`: the code and service only, never the warning's message.
    fn from((change, at): (&HealthChange, OffsetDateTime)) -> Self {
        let (kind, verb, code, service) = match change {
            HealthChange::Degraded { code, service } => (ServerKind::VaultDegraded, "degraded", code, service),
            HealthChange::Recovered { code, service } => (ServerKind::VaultRecovered, "recovered", code, service),
        };
        let field = |name: &str, value: &str| {
            FieldName::try_from(name)
                .ok()
                .map(|name| (name, FieldValue::from(value)))
        };
        Self {
            kind: KindName::from(kind),
            severity: kind.default_severity(),
            at,
            entity: resolve_entity(None, None, None),
            subject: None,
            actor: None,
            fields: [field("code", code), field("service", service)]
                .into_iter()
                .flatten()
                .collect(),
            heading: format!("Vault {verb}: {code}"),
            content: None,
        }
    }
}

#[cfg(test)]
#[path = "health_test.rs"]
mod tests;
