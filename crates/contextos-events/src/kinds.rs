use std::fmt;

use serde::{Deserialize, Serialize};

use crate::EventsError;

/// The longest kind name accepted, which keeps a kind usable as a file name and a frontmatter value.
const MAX_KIND_NAME_LEN: usize = 100;

/// How much attention an event asks for: neutral, amber, or red.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// Something happened worth seeing.
    Info,
    /// Something is degraded or needs a person, and work continues.
    Warning,
    /// Something has stopped working.
    Error,
}

impl Severity {
    /// Every severity, least to most urgent.
    pub const ALL: [Self; 3] = [Self::Info, Self::Warning, Self::Error];

    /// The name the severity has in frontmatter.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Warning => "warning",
            Self::Error => "error",
        }
    }
}

impl TryFrom<&str> for Severity {
    type Error = EventsError;

    fn try_from(value: &str) -> Result<Self, EventsError> {
        Self::ALL
            .into_iter()
            .find(|severity| severity.as_str() == value)
            .ok_or_else(|| EventsError::InvalidSeverity {
                value: value.to_owned(),
            })
    }
}

impl fmt::Display for Severity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A kind of event the server itself fires from the MCP call that causes it. No client can record one.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ServerKind {
    TaskCreated,
    TaskStateChanged,
    TaskReassigned,
    TaskClaimLapsed,
    TaskUnknownState,
    TaskMoved,
    MemberStateChanged,
    VaultDegraded,
    VaultRecovered,
}

impl ServerKind {
    /// Every server-fired kind, in the order task events are fired.
    pub const ALL: [Self; 9] = [
        Self::TaskCreated,
        Self::TaskStateChanged,
        Self::TaskReassigned,
        Self::TaskClaimLapsed,
        Self::TaskUnknownState,
        Self::TaskMoved,
        Self::MemberStateChanged,
        Self::VaultDegraded,
        Self::VaultRecovered,
    ];

    /// The kind's name as written in an event note.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::TaskCreated => "task.created",
            Self::TaskStateChanged => "task.state-changed",
            Self::TaskReassigned => "task.reassigned",
            Self::TaskClaimLapsed => "task.claim-lapsed",
            Self::TaskUnknownState => "task.unknown-state",
            Self::TaskMoved => "task.moved",
            Self::MemberStateChanged => "member.state-changed",
            Self::VaultDegraded => "vault.degraded",
            Self::VaultRecovered => "vault.recovered",
        }
    }

    /// The fields the kind's events carry beyond the common event fields, in the order a reader shows them.
    #[must_use]
    pub const fn fields(self) -> &'static [&'static str] {
        match self {
            Self::TaskCreated => &["task_type", "state", "assignee", "origin"],
            Self::TaskStateChanged => &["from", "to", "task_type", "assignee", "claimed_by"],
            Self::TaskReassigned | Self::TaskMoved => &["from", "to"],
            Self::TaskClaimLapsed => &["claimed_by", "claimed_until"],
            Self::TaskUnknownState => &["state", "task_type"],
            Self::MemberStateChanged => &["member", "from", "to", "reason"],
            Self::VaultDegraded | Self::VaultRecovered => &["code", "service"],
        }
    }

    /// The severity the kind carries unless its meaning varies (a member blocked is a warning, for one).
    #[must_use]
    pub const fn default_severity(self) -> Severity {
        match self {
            Self::TaskClaimLapsed | Self::TaskUnknownState | Self::VaultDegraded => Severity::Warning,
            Self::TaskCreated
            | Self::TaskStateChanged
            | Self::TaskReassigned
            | Self::TaskMoved
            | Self::MemberStateChanged
            | Self::VaultRecovered => Severity::Info,
        }
    }
}

/// The kinds the scaffold's own scheduler, session, and agent skills record, which the product ships beside the kinds
/// the server fires.
pub const SCAFFOLD_KINDS: [&str; 7] = [
    "job.failed",
    "job.suspended",
    "job.recovered",
    "job.retired",
    "session.run-started",
    "session.run-finished",
    "agent.delegated",
];

/// Whether `kind` is one the product ships: fired by the server or recorded by the scaffold. Any other kind is
/// background activity from the vault or an external source. The comparison is exact.
#[must_use]
pub fn is_shipped_kind(kind: &str) -> bool {
    SCAFFOLD_KINDS.contains(&kind) || ServerKind::ALL.iter().any(|server| server.as_str() == kind)
}

impl TryFrom<&str> for ServerKind {
    type Error = EventsError;

    fn try_from(value: &str) -> Result<Self, EventsError> {
        Self::ALL
            .into_iter()
            .find(|kind| kind.as_str() == value)
            .ok_or_else(|| EventsError::UnknownKind {
                kind: Some(value.to_owned()),
            })
    }
}

impl From<ServerKind> for KindName {
    fn from(value: ServerKind) -> Self {
        Self(value.as_str().to_owned())
    }
}

/// A validated event kind name: lower-case dot-separated segments of letters, digits, and hyphens.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct KindName(String);

impl KindName {
    /// The name as written in frontmatter.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<&str> for KindName {
    type Error = EventsError;

    fn try_from(value: &str) -> Result<Self, EventsError> {
        let segment_ok = |segment: &str| {
            !segment.is_empty()
                && !segment.starts_with('-')
                && !segment.ends_with('-')
                && segment
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        };
        if value.len() <= MAX_KIND_NAME_LEN && value.split('.').all(segment_ok) {
            Ok(Self(value.to_owned()))
        } else {
            Err(EventsError::InvalidKindName {
                value: value.to_owned(),
            })
        }
    }
}

impl AsRef<str> for KindName {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for KindName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
#[path = "kinds_test.rs"]
mod tests;
