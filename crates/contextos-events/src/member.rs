use time::OffsetDateTime;

use crate::{EventRecord, EventSubject, FieldName, FieldValue, KindName, ServerKind, Severity, resolve_entity};

/// The state that draws attention: a member who cannot continue.
const BLOCKED: &str = "blocked";

/// One team member's state transition, as `agent_state_change` records it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MemberStateChange {
    /// The member's slug.
    pub member: String,
    /// The state before, absent on the member's first transition.
    pub from: Option<String>,
    pub to: String,
    pub reason: Option<String>,
    /// Whether `registry/agents/{member}.md` exists, so the event can link it.
    pub has_definition: bool,
}

impl From<(&MemberStateChange, OffsetDateTime)> for EventRecord {
    /// The `member.state-changed` event for a transition at `at`: entity `vault`, since a member works across
    /// entities, and a warning when the member became blocked.
    fn from((change, at): (&MemberStateChange, OffsetDateTime)) -> Self {
        let field = |name: &str, value: &Option<String>| {
            let value = value.as_deref()?;
            Some((FieldName::try_from(name).ok()?, FieldValue::from(value)))
        };
        let heading = change.from.as_ref().map_or_else(
            || format!("{}: {}", change.member, change.to),
            |from| format!("{}: {from} \u{2192} {}", change.member, change.to),
        );
        Self {
            kind: KindName::from(ServerKind::MemberStateChanged),
            severity: if change.to == BLOCKED {
                Severity::Warning
            } else {
                ServerKind::MemberStateChanged.default_severity()
            },
            at,
            entity: resolve_entity(None, None, None),
            subject: change
                .has_definition
                .then(|| EventSubject::for_note(&format!("registry/agents/{}.md", change.member))),
            actor: None,
            fields: [
                field("member", &Some(change.member.clone())),
                field("from", &change.from),
                field("to", &Some(change.to.clone())),
                field("reason", &change.reason),
            ]
            .into_iter()
            .flatten()
            .collect(),
            heading,
            content: None,
        }
    }
}

#[cfg(test)]
#[path = "member_test.rs"]
mod tests;
