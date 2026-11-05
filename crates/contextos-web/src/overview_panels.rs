//! What the vault Overview's Bridge, System status, and Inputs panels show, built from what the vault already records:
//! the team's chief of staff and their liveness, the last day of events, the Inbox, the scheduled jobs, and the
//! sources that record events. Nothing here invents a summary or a metric the vault does not hold.

use std::collections::BTreeMap;

use askama::Template;
use time::OffsetDateTime;

use crate::agents_liveness::MemberLiveness;
use crate::signals_event::Severity;
use crate::signals_view::format_time;
use crate::team::TeamMember;

/// The member the team flags as chief of staff, the first when several are flagged.
#[must_use]
pub fn chief_of_staff(team: &[TeamMember]) -> Option<&TeamMember> {
    team.iter().find(|member| member.chief_of_staff)
}

/// One labelled fact on a card.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Fact {
    pub label: &'static str,
    pub value: String,
}

/// What the Bridge card is built from.
pub struct ChiefFacts<'a> {
    pub member: &'a TeamMember,
    /// The member's liveness, `None` when it could not be read.
    pub liveness: Option<&'a MemberLiveness>,
    /// How many open tasks are assigned to the member, `None` when the tasks could not be read.
    pub assigned: Option<usize>,
}

/// The Bridge panel's chief-of-staff card.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChiefCard {
    pub name: String,
    /// The member's state, as the Agents screen names it.
    pub state: &'static str,
    pub facts: Vec<Fact>,
}

impl From<ChiefFacts<'_>> for ChiefCard {
    fn from(input: ChiefFacts<'_>) -> Self {
        let unknown = || "Unknown".to_owned();
        let last_session = input.liveness.map_or_else(unknown, |liveness| {
            liveness.last_finish.map_or_else(
                || "No session recorded".to_owned(),
                |(at, severity)| format!("{} ({})", format_time(at), session_outcome(severity)),
            )
        });
        Self {
            name: input.member.name.clone(),
            state: input.member.state.label(),
            facts: vec![
                Fact {
                    label: "Open tasks",
                    value: input.assigned.map_or_else(unknown, |assigned| assigned.to_string()),
                },
                Fact {
                    label: "Claimed",
                    value: input
                        .liveness
                        .map_or_else(unknown, |liveness| liveness.claimed.to_string()),
                },
                Fact {
                    label: "Last session",
                    value: last_session,
                },
                Fact {
                    label: "Lapsed claims",
                    value: input
                        .liveness
                        .map_or_else(unknown, |liveness| liveness.lapsed.len().to_string()),
                },
            ],
        }
    }
}

/// How a session ended, from the severity of the event that finished it.
const fn session_outcome(severity: Severity) -> &'static str {
    match severity {
        Severity::Info => "finished",
        Severity::Warning => "finished with warnings",
        Severity::Error => "failed",
    }
}

/// The Bridge panel.
#[derive(Template)]
#[template(path = "overview_bridge.html")]
pub struct BridgeView {
    /// Where the panel reloads itself from when the live stream says something may have changed.
    pub refresh_href: String,
    /// The chief of staff's card, `None` when the team flags no one.
    pub chief: Option<ChiefCard>,
    pub agents_href: String,
    pub decisions_href: String,
}

/// The last day of events, by severity.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct EventTally {
    pub info: usize,
    pub warning: usize,
    pub error: usize,
}

impl EventTally {
    #[must_use]
    pub const fn total(&self) -> usize {
        self.info + self.warning + self.error
    }
}

impl From<&[Severity]> for EventTally {
    fn from(severities: &[Severity]) -> Self {
        severities.iter().fold(Self::default(), |mut tally, severity| {
            match severity {
                Severity::Info => tally.info += 1,
                Severity::Warning => tally.warning += 1,
                Severity::Error => tally.error += 1,
            }
            tally
        })
    }
}

/// The System status panel.
#[derive(Template)]
#[template(path = "overview_status.html")]
pub struct StatusView {
    /// Where the panel reloads itself from when the live stream says something may have changed.
    pub refresh_href: String,
    pub status: crate::rendering::page::McpStatus,
    /// The last day of events, `None` when they could not be read.
    pub tally: Option<EventTally>,
    /// Whether the last day held more events than were counted.
    pub truncated: bool,
    pub health_summary_href: String,
    pub health_href: String,
    pub signals_href: String,
    pub settings_href: String,
}

/// A source that records events: one recorded kind and its newest event.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecordedSource {
    pub kind: String,
    pub label: String,
    pub latest: OffsetDateTime,
    pub severity: Severity,
    pub href: String,
}

/// The newest event of each kind among `events` (kind, time, severity) that the server does not fire itself, by kind.
#[must_use]
pub fn newest_per_kind(
    events: Vec<(String, OffsetDateTime, Severity)>,
) -> BTreeMap<String, (OffsetDateTime, Severity)> {
    let mut newest: BTreeMap<String, (OffsetDateTime, Severity)> = BTreeMap::new();
    for (kind, at, severity) in events {
        if contextos_events::ServerKind::ALL
            .iter()
            .any(|server| server.as_str() == kind)
        {
            continue;
        }
        if newest.get(&kind).is_none_or(|(seen, _)| at > *seen) {
            newest.insert(kind, (at, severity));
        }
    }
    newest
}

/// One row of the Inputs panel.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InputRow {
    pub icon: &'static str,
    pub label: String,
    pub meta: String,
    /// The status dot's class: `working`, `idle`, `attention`, `error`, or `unknown`.
    pub state: &'static str,
    pub href: String,
}

impl InputRow {
    /// The Inbox: how many items wait to be processed, `None` when it could not be read.
    #[must_use]
    pub fn inbox(waiting: Option<usize>, href: String) -> Self {
        let (meta, state) = match waiting {
            None => ("Unknown".to_owned(), "unknown"),
            Some(0) => ("Nothing waiting".to_owned(), "idle"),
            Some(count) => (format!("{count} waiting"), "working"),
        };
        Self {
            icon: "inbox",
            label: "Inbox".to_owned(),
            meta,
            state,
            href,
        }
    }

    /// A source that records events, by its newest event.
    #[must_use]
    pub fn recorded(source: &RecordedSource) -> Self {
        Self {
            icon: "radio",
            label: source.label.clone(),
            meta: format_time(source.latest),
            state: match source.severity {
                Severity::Info => "idle",
                Severity::Warning => "attention",
                Severity::Error => "error",
            },
            href: source.href.clone(),
        }
    }
}

/// The Inputs panel.
#[derive(Template)]
#[template(path = "overview_inputs.html")]
pub struct InputsView {
    /// Where the panel reloads itself from when the live stream says something may have changed.
    pub refresh_href: String,
    pub rows: Vec<InputRow>,
    /// Whether a source could not be read, so the list may be missing one.
    pub incomplete: bool,
    pub signals_href: String,
}

#[cfg(test)]
#[path = "overview_panels_test.rs"]
mod tests;
