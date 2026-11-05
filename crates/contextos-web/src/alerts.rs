//! The alerts the top-bar bell and the Overview's Alert rail list: open tasks past their due date, open questions, and
//! the warnings and errors nothing has cleared, most recent first. Every alert is read from what the vault already
//! holds (its task records, its questions, and its events); nothing of its own is stored. A task waiting on something
//! is not an alert: waiting is a normal state that names something outside the vault.

use time::Date;
use time::macros::format_description;

use crate::signals_event::Severity;
use crate::signals_view::EventLine;

/// What raised an alert.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AlertKind {
    /// A warning or error event that nothing since has cleared.
    Attention(Severity),
    /// An open task whose due date has passed.
    Overdue,
    /// A question that is still open.
    Question,
}

/// One alert, ready to list.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Alert {
    pub kind: AlertKind,
    pub title: String,
    pub detail: String,
    /// When the alert's source last changed, as a reader sees it.
    pub when: String,
    /// Where the alert's subject opens, when it has a page.
    pub href: Option<String>,
    /// The moment the alert sorts by, as an ISO 8601 date or time so text order is time order.
    sort_key: String,
}

impl Alert {
    /// The icon the alert is listed with.
    #[must_use]
    pub const fn icon(&self) -> &'static str {
        match self.kind {
            AlertKind::Attention(Severity::Error) => "octagon-alert",
            AlertKind::Attention(_) => "circle-alert",
            AlertKind::Overdue => "calendar",
            AlertKind::Question => "circle-help",
        }
    }

    /// The tone the alert's icon takes: an error is red, everything else amber.
    #[must_use]
    pub const fn tone(&self) -> &'static str {
        match self.kind {
            AlertKind::Attention(Severity::Error) => "error",
            _ => "attention",
        }
    }
}

/// An open task, as the alerts read it.
#[derive(Clone, Debug, Default)]
pub struct OpenTask {
    pub id: String,
    pub title: String,
    /// The `due` field as written.
    pub due: Option<String>,
    pub href: String,
}

impl OpenTask {
    /// Whether the task's due date is before `today`. A `due` that does not begin with a `YYYY-MM-DD` date is never
    /// read as one.
    #[must_use]
    pub fn is_overdue(&self, today: Date) -> bool {
        self.due_date().is_some_and(|due| due < today)
    }

    fn due_date(&self) -> Option<Date> {
        let due = self.due.as_deref()?.trim();
        Date::parse(due.get(..10)?, format_description!("[year]-[month]-[day]")).ok()
    }
}

/// An open question, as the alerts read it.
#[derive(Clone, Debug, Default)]
pub struct OpenQuestion {
    pub id: String,
    pub title: String,
    /// When it last changed, as written.
    pub updated: Option<String>,
    pub href: String,
}

/// `id` then `title`, or whichever of them there is.
fn keyed(id: &str, title: &str) -> String {
    match (id.trim(), title.trim()) {
        ("", title) => title.to_owned(),
        (id, "") => id.to_owned(),
        (id, title) => format!("{id} {title}"),
    }
}

/// `href` unless it is empty.
fn link(href: String) -> Option<String> {
    (!href.is_empty()).then_some(href)
}

impl From<OpenTask> for Alert {
    /// An overdue task: the due date is when it began to need attention.
    fn from(task: OpenTask) -> Self {
        let due = task
            .due_date()
            .map(|date| date.to_string())
            .or_else(|| task.due.clone())
            .unwrap_or_default();
        Self {
            kind: AlertKind::Overdue,
            title: keyed(&task.id, &task.title),
            detail: format!("Overdue since {due}"),
            when: due.clone(),
            href: link(task.href),
            sort_key: due,
        }
    }
}

impl From<OpenQuestion> for Alert {
    fn from(question: OpenQuestion) -> Self {
        let updated = question.updated.unwrap_or_default();
        Self {
            kind: AlertKind::Question,
            title: keyed(&question.id, &question.title),
            detail: "Open question".to_owned(),
            when: crate::register_records::display_date(&updated),
            href: link(question.href),
            sort_key: updated,
        }
    }
}

impl From<&EventLine> for Alert {
    fn from(line: &EventLine) -> Self {
        let severity = Severity::try_from(line.severity).unwrap_or(Severity::Warning);
        let title = if line.subject_label.is_empty() {
            line.label.clone()
        } else {
            format!("{}: {}", line.label, line.subject_label)
        };
        Self {
            kind: AlertKind::Attention(severity),
            title,
            detail: line.detail.clone(),
            when: line.time.clone(),
            href: line.subject_href.clone(),
            sort_key: line.iso.clone(),
        }
    }
}

/// `alerts` with the most recent first; alerts of the same moment keep a stable order by title.
#[must_use]
pub fn most_recent_first(mut alerts: Vec<Alert>) -> Vec<Alert> {
    alerts.sort_by(|left, right| {
        right
            .sort_key
            .cmp(&left.sort_key)
            .then_with(|| left.title.cmp(&right.title))
    });
    alerts
}

#[cfg(test)]
#[path = "alerts_test.rs"]
mod tests;
