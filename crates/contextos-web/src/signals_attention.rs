//! What needs attention: the warning and error events that nothing since has cleared. A pure reading of events, so the
//! Signals screen, the Tasks indicator, and the live stream's refresh all apply the same rules.
//!
//! The clearing rules themselves live in `contextos-events` ([`Occurrence`]), the single definition the retention sweep
//! also uses; this module only adapts a [`SignalEvent`] to them.

use contextos_events::Occurrence;

use crate::signals_event::{Severity, SignalEvent};

impl From<&SignalEvent> for Occurrence {
    fn from(event: &SignalEvent) -> Self {
        Self {
            path: event.path.clone(),
            kind: event.kind.clone(),
            severity: event.severity,
            at: event.at,
            subject: event.subject_identity(),
            code: event.field("code").map(str::to_owned),
            workflow: event.field("workflow").map(str::to_owned),
            job: event.field("job").map(str::to_owned),
        }
    }
}

/// The events among `candidates` that need attention and that nothing in `pool` has cleared, in the order given.
///
/// `candidates` may hold events of any severity; only warnings and errors can need attention. `pool` is every event
/// that may clear one, which may include the candidates themselves.
#[must_use]
pub fn open_attention<'a>(candidates: &'a [SignalEvent], pool: &[SignalEvent]) -> Vec<&'a SignalEvent> {
    let pool: Vec<Occurrence> = pool.iter().map(Occurrence::from).collect();
    candidates
        .iter()
        .filter(|event| Occurrence::from(*event).is_open(&pool))
        .collect()
}

/// The severity a task's indicator shows: that of the most recent event about the task when it is a warning or an
/// error, and none when the task has no events or its most recent event is information. A later event of any kind
/// clears it, because it becomes the most recent, except `task.moved`: moving a task fixes nothing about it.
#[must_use]
pub fn task_indicator(events_about_the_task: &[SignalEvent]) -> Option<Severity> {
    events_about_the_task
        .iter()
        .filter(|event| event.kind != contextos_events::TASK_MOVED)
        .max_by_key(|event| event.cursor())
        .map(|event| event.severity)
        .filter(|severity| *severity != Severity::Info)
}

/// Whether an event arriving could change what a page derives from events, so open pages should reload: it raises
/// attention itself, it may clear some (it is about a subject, or is a recovery), or it is a session or member event,
/// which the Agents screen shows.
#[must_use]
pub fn may_change_attention(event: &SignalEvent) -> bool {
    event.severity != Severity::Info
        || event.subject.is_some()
        || event.kind.ends_with(".recovered")
        || event.kind.starts_with("session.")
        || event.kind.starts_with("member.")
}

#[cfg(test)]
#[path = "signals_attention_test.rs"]
mod tests;
