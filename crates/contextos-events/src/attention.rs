//! What needs attention: the one definition of when a warning or error event is open and when something later has
//! cleared it, shared by `ContextOS Web` (the Signals attention panel, the sidebar count, the Tasks indicator) and by the
//! retention sweep, so a cleared warning never reappears as open because the event that cleared it was swept.
//!
//! An event that has a subject is cleared by any later event about the same subject, except `task.moved`: moving a
//! task fixes nothing about it. A record of a keyed base is the same subject wherever it is filed, since it is
//! identified by its base and key, never its path; any other note is identified by its path. Two kinds carry their own
//! clearing events instead: `vault.degraded` is cleared by a later `vault.recovered` with the same `code`, and
//! `job.failed` and `job.suspended` by a later `job.recovered` for the same workflow and job. An event with no subject
//! is never cleared; it stays open until it is swept or deleted.

use serde_json::{Map, Value};
use time::format_description::well_known::Rfc3339;
use time::{OffsetDateTime, UtcOffset};

use crate::Severity;

/// The kind that clears a `vault.degraded` event with the same `code`.
pub const VAULT_RECOVERED: &str = "vault.recovered";
/// The kind that clears a `job.failed` or `job.suspended` event for the same workflow and job.
pub const JOB_RECOVERED: &str = "job.recovered";
/// The kind that records a task being filed elsewhere, which clears nothing.
pub const TASK_MOVED: &str = crate::ServerKind::TaskMoved.as_str();

/// What an event is about, as the clearing rules compare it.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum SubjectIdentity {
    /// A record of a keyed base: its base's vault path and its key, upper-cased, since keys compare without case.
    Record { base: String, key: String },
    /// Any other note: its vault path, without a Markdown extension.
    Note { path: String },
}

/// An event's subject fields as written: `subject`, `subject_id`, `subject_path`, and `subject_base`.
#[derive(Clone, Copy, Debug, Default)]
pub struct SubjectFields<'a> {
    pub link: Option<&'a str>,
    pub id: Option<&'a str>,
    pub path: Option<&'a str>,
    pub base: Option<&'a str>,
}

/// Subject fields that name no subject.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NoSubject;

impl TryFrom<SubjectFields<'_>> for SubjectIdentity {
    type Error = NoSubject;

    /// A record when both its base and key are given, else the note its recorded path names, else the note its link
    /// names. Blank values count as absent.
    fn try_from(fields: SubjectFields<'_>) -> Result<Self, NoSubject> {
        fn present(value: Option<&str>) -> Option<&str> {
            value.map(str::trim).filter(|value| !value.is_empty())
        }
        if let (Some(base), Some(key)) = (present(fields.base), present(fields.id)) {
            return Ok(Self::Record {
                base: base.to_owned(),
                key: key.to_uppercase(),
            });
        }
        if let Some(path) = present(fields.path) {
            return Ok(Self::Note {
                path: path.strip_suffix(".md").unwrap_or(path).to_owned(),
            });
        }
        present(fields.link)
            .and_then(link_target)
            .map(|path| Self::Note { path })
            .ok_or(NoSubject)
    }
}

/// The facts of an event that decide whether it is open: enough to apply the clearing rules and nothing else.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Occurrence {
    /// The note's vault path, which orders events that share a second.
    pub path: String,
    /// The kind name.
    pub kind: String,
    /// How serious the event is.
    pub severity: Severity,
    /// When it happened, in UTC.
    pub at: OffsetDateTime,
    /// What the event is about.
    pub subject: Option<SubjectIdentity>,
    /// The kind's `code` field.
    pub code: Option<String>,
    /// The kind's `workflow` field.
    pub workflow: Option<String>,
    /// The kind's `job` field.
    pub job: Option<String>,
}

/// A note's frontmatter that is not an event the rules can judge.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NotJudgeable;

/// The vault path a `[[target|label]]` link names, without a `.md`, or `None` for text that is not a link.
#[must_use]
pub fn link_target(link: &str) -> Option<String> {
    let inner = link.trim().strip_prefix("[[")?.strip_suffix("]]")?;
    let target = inner.split(['|', '#']).next()?.trim();
    if target.is_empty() {
        return None;
    }
    Some(target.strip_suffix(".md").unwrap_or(target).to_owned())
}

fn text_field(frontmatter: &Map<String, Value>, name: &str) -> Option<String> {
    frontmatter
        .get(name)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

impl TryFrom<(&str, &Map<String, Value>)> for Occurrence {
    type Error = NotJudgeable;

    /// Reads the occurrence of the event note at `path` from its frontmatter; the kind, a known severity, and an
    /// RFC 3339 `at` are required.
    fn try_from((path, frontmatter): (&str, &Map<String, Value>)) -> Result<Self, NotJudgeable> {
        let kind = text_field(frontmatter, "kind").ok_or(NotJudgeable)?;
        let severity = text_field(frontmatter, "severity").ok_or(NotJudgeable)?;
        let severity = Severity::try_from(severity.as_str()).map_err(|_| NotJudgeable)?;
        let at = text_field(frontmatter, "at").ok_or(NotJudgeable)?;
        let at = OffsetDateTime::parse(&at, &Rfc3339).map_err(|_| NotJudgeable)?;
        Ok(Self {
            path: path.to_owned(),
            kind,
            severity,
            at: at.to_offset(UtcOffset::UTC),
            subject: SubjectIdentity::try_from(SubjectFields {
                link: text_field(frontmatter, "subject").as_deref(),
                id: text_field(frontmatter, "subject_id").as_deref(),
                path: text_field(frontmatter, "subject_path").as_deref(),
                base: text_field(frontmatter, "subject_base").as_deref(),
            })
            .ok(),
            code: text_field(frontmatter, "code"),
            workflow: text_field(frontmatter, "workflow"),
            job: text_field(frontmatter, "job"),
        })
    }
}

impl Occurrence {
    /// Whether the event asks for attention at all: a warning or an error.
    #[must_use]
    pub const fn needs_attention(&self) -> bool {
        !matches!(self.severity, Severity::Info)
    }

    fn is_later_than(&self, other: &Self) -> bool {
        (self.at, &self.path) > (other.at, &other.path)
    }

    /// Whether this event happened in the same second as `other` or after it. An explicit recovery is made in answer to
    /// the failure it recovers from, so one in the failure's own second follows it whatever their paths sort as.
    fn is_not_earlier_than(&self, other: &Self) -> bool {
        let second = |at: OffsetDateTime| at.replace_nanosecond(0).unwrap_or(at);
        second(self.at) >= second(other.at)
    }

    /// Whether `later` ends the attention this event asked for.
    #[must_use]
    pub fn is_cleared_by(&self, later: &Self) -> bool {
        match self.kind.as_str() {
            "vault.degraded" => {
                later.kind == VAULT_RECOVERED && later.code == self.code && later.is_not_earlier_than(self)
            }
            "job.failed" | "job.suspended" => {
                later.kind == JOB_RECOVERED && self.is_same_job(later) && later.is_not_earlier_than(self)
            }
            _ => {
                self.subject.is_some()
                    && self.subject == later.subject
                    && later.kind != TASK_MOVED
                    && later.is_later_than(self)
            }
        }
    }

    /// Whether both events are about one job: the same workflow and the same job slug, both of which must be named,
    /// since two workflows may share a job slug.
    fn is_same_job(&self, other: &Self) -> bool {
        self.workflow.is_some() && self.job.is_some() && self.workflow == other.workflow && self.job == other.job
    }

    /// Whether the event needs attention and nothing in `pool` has cleared it. `pool` is every event that may clear
    /// it, and may include the event itself.
    #[must_use]
    pub fn is_open(&self, pool: &[Self]) -> bool {
        self.needs_attention() && !pool.iter().any(|later| self.is_cleared_by(later))
    }
}

#[cfg(test)]
#[path = "attention_test.rs"]
mod tests;
